//! 초대 코드와 봉인 blob (§4.3.3, §4.3.6, D27).
//!
//! 코드(60-bit) → Argon2id(salt=workspace_id) → K_code → HKDF 분기 locator/K_seal.
//! blob 에는 GK 를 절대 넣지 않는다(D27) — grant_sk 등 admission 비밀만 봉인.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use sb_proto::log::{GrantCert, LogEntry};

use crate::identity::{Identity, IdentityPublic};
use sb_proto::params::{ARGON2_MEM_KIB, ARGON2_PAR, ARGON2_TIME, INVITE_CODE_CHARS};
use sb_proto::Locator;

use crate::aead::{xopen, xseal};
use crate::hash::{INFO_INV_LOCATOR, INFO_INV_SEAL};
use crate::CryptoError;

/// Crockford Base32 알파벳 (I/L/O/U 제외).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 60-bit 랜덤 초대 코드 생성 → 정규형 12자(대문자, 대시 없음).
pub fn generate_code() -> Zeroizing<String> {
    let mut buf = [0u8; 8];
    OsRng.fill_bytes(&mut buf);
    let bits: u64 = u64::from_le_bytes(buf) & ((1u64 << 60) - 1); // 하위 60비트
    let mut s = String::with_capacity(INVITE_CODE_CHARS);
    for i in 0..INVITE_CODE_CHARS {
        let shift = 5 * (INVITE_CODE_CHARS - 1 - i);
        let idx = ((bits >> shift) & 0x1f) as usize;
        s.push(CROCKFORD[idx] as char);
    }
    Zeroizing::new(s)
}

/// 표시용 그루핑 "XXXX-XXXX-XXXX".
pub fn format_display(code: &str) -> String {
    let c = canonicalize(code);
    let b = c.as_bytes();
    if b.len() != 12 {
        return c;
    }
    format!(
        "{}-{}-{}",
        std::str::from_utf8(&b[0..4]).unwrap(),
        std::str::from_utf8(&b[4..8]).unwrap(),
        std::str::from_utf8(&b[8..12]).unwrap()
    )
}

/// 사용자 입력 정규화 — 대문자화, 대시/공백 제거, Crockford 혼동 문자 매핑(I/L→1, O→0).
pub fn canonicalize(input: &str) -> String {
    let mut out = String::with_capacity(INVITE_CODE_CHARS);
    for ch in input.chars() {
        let u = ch.to_ascii_uppercase();
        match u {
            '-' | ' ' => continue,
            'O' => out.push('0'),
            'I' | 'L' => out.push('1'),
            'U' => out.push('V'), // Crockford: U 오타 방지
            c => out.push(c),
        }
    }
    out
}

/// 코드 + workspace_id → (locator, K_seal). Argon2id 메모리 경도(§4.3.6).
pub fn derive(code: &str, workspace_id: &[u8; 32]) -> Result<(Locator, Zeroizing<[u8; 32]>), CryptoError> {
    let canon = canonicalize(code);
    let params =
        Params::new(ARGON2_MEM_KIB, ARGON2_TIME, ARGON2_PAR, Some(32)).map_err(|_| CryptoError::Kdf)?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut k_code = Zeroizing::new([0u8; 32]);
    a2.hash_password_into(canon.as_bytes(), workspace_id, &mut *k_code)
        .map_err(|_| CryptoError::Kdf)?;

    let hk = Hkdf::<Sha256>::new(None, &*k_code);
    let mut locator = [0u8; 32];
    hk.expand(INFO_INV_LOCATOR, &mut locator)
        .map_err(|_| CryptoError::Kdf)?;
    let mut k_seal = Zeroizing::new([0u8; 32]);
    hk.expand(INFO_INV_SEAL, &mut *k_seal)
        .map_err(|_| CryptoError::Kdf)?;
    Ok((locator, k_seal))
}

/// 초대별 일회용 grant 키쌍 (§4.3.3 step 3).
pub struct GrantKeypair {
    pub sk: SigningKey,
    /// P-256 SPKI DER (grant_cert.grant_pk).
    pub pk_der: Vec<u8>,
}

/// grant 키쌍 생성. `sk` 는 초대 blob 안에 봉인되어 전달, Add 확정 시 파기.
pub fn generate_grant() -> GrantKeypair {
    let sk = SigningKey::random(&mut OsRng);
    let pk_der = VerifyingKey::from(&sk)
        .to_public_key_der()
        .expect("spki")
        .as_bytes()
        .to_vec();
    GrantKeypair { sk, pk_der }
}

/// 초대 blob 평문 (§4.3.3 step 4). **GK 미포함**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteSecret {
    /// 일회용 grant 개인키 (PKCS#8 DER).
    pub grant_sk_pkcs8: Vec<u8>,
    pub grant_cert: GrantCert,
    pub workspace_id: [u8; 32],
    /// 발급 시점 로그 head 해시 — 조인자 freshness floor.
    pub log_head_hash: [u8; 32],
    /// 서버 TLS cert 지문(SHA-256 SPKI) — TOFU 소급 검증·pinning 확정.
    pub server_fp: [u8; 32],
}

/// blob 봉인 = XChaCha20-Poly1305(K_seal). aad = workspace_id(가짜 워크스페이스 유인 방어).
pub fn seal_blob(k_seal: &[u8; 32], secret: &InviteSecret) -> Result<Vec<u8>, CryptoError> {
    use zeroize::Zeroize;
    let mut pt = Vec::new();
    ciborium::into_writer(secret, &mut pt).map_err(|_| CryptoError::Encode)?;
    let r = xseal(k_seal, &secret.workspace_id, &pt);
    pt.zeroize();
    r
}

/// blob 개봉. aad = 기대 workspace_id (조인자가 코드로 자체 계산한 값과 일치해야).
pub fn open_blob(
    k_seal: &[u8; 32],
    workspace_id: &[u8; 32],
    sealed: &[u8],
) -> Result<InviteSecret, CryptoError> {
    let pt = xopen(k_seal, workspace_id, sealed)?;
    let secret: InviteSecret = ciborium::from_reader(&pt[..]).map_err(|_| CryptoError::Decode)?;
    Ok(secret)
}

/// 초대 발급 원스톱 (§4.3.3). 반환 `(코드, locator, blob)`.
/// `expires_at` 은 절대 시각(ms) — 클록은 호출자(앱)가 제공.
pub fn make_invite(
    sponsor: &Identity,
    workspace_id: [u8; 32],
    log_head_hash: [u8; 32],
    server_fp: [u8; 32],
    expires_at: u64,
) -> Result<(Zeroizing<String>, Locator, Vec<u8>), CryptoError> {
    let code = generate_code();
    let (locator, k_seal) = derive(&code, &workspace_id)?;
    let grant = generate_grant();
    let grant_cert = crate::wslog::build_grant_cert(sponsor, grant.pk_der, expires_at, workspace_id);
    let grant_sk_pkcs8 = grant
        .sk
        .to_pkcs8_der()
        .map_err(|_| CryptoError::KeyEncoding)?
        .as_bytes()
        .to_vec();
    let secret = InviteSecret {
        grant_sk_pkcs8,
        grant_cert,
        workspace_id,
        log_head_hash,
        server_fp,
    };
    let blob = seal_blob(&k_seal, &secret)?;
    Ok((code, locator, blob))
}

/// 조인 원스톱 (§4.3.4). 코드+blob 을 열어 grant_sk 로 Add 엔트리를 만든다.
/// 반환 `(Add 엔트리, InviteSecret)` — secret 의 server_fp/log_head_hash 로 조인자가 pinning·검증.
pub fn build_add_from_blob(
    code: &str,
    workspace_id: [u8; 32],
    blob: &[u8],
    joiner: &IdentityPublic,
    prev_hash: [u8; 32],
    seq: u64,
    joined_at: u64,
) -> Result<(LogEntry, InviteSecret), CryptoError> {
    let (_locator, k_seal) = derive(code, &workspace_id)?;
    let secret = open_blob(&k_seal, &workspace_id, blob)?;
    let grant_sk =
        SigningKey::from_pkcs8_der(&secret.grant_sk_pkcs8).map_err(|_| CryptoError::KeyEncoding)?;
    let add = crate::wslog::build_add(
        &grant_sk,
        secret.grant_cert.clone(),
        joiner,
        prev_hash,
        seq,
        joined_at,
    );
    Ok((add, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_12_crockford_chars() {
        let code = generate_code();
        assert_eq!(code.len(), 12);
        assert!(code.bytes().all(|b| CROCKFORD.contains(&b)));
    }

    #[test]
    fn canonicalize_rules() {
        assert_eq!(canonicalize("ABCD-EFGH-JKMN"), "ABCDEFGHJKMN");
        assert_eq!(canonicalize("o i l"), "011");
        assert_eq!(canonicalize("abc"), "ABC");
    }

    #[test]
    fn derive_deterministic() {
        let wid = [5u8; 32];
        let (l1, s1) = derive("ABCD-EFGH-JKMN", &wid).unwrap();
        let (l2, s2) = derive("abcdefghjkmn", &wid).unwrap(); // 정규화로 동일
        assert_eq!(l1, l2);
        assert_eq!(*s1, *s2);
    }

    #[test]
    fn derive_varies_with_workspace() {
        let (l1, _) = derive("ABCDEFGHJKMN", &[1u8; 32]).unwrap();
        let (l2, _) = derive("ABCDEFGHJKMN", &[2u8; 32]).unwrap();
        assert_ne!(l1, l2, "salt=workspace_id 이므로 워크스페이스마다 다름");
    }

    #[test]
    fn make_invite_then_build_add_roundtrip() {
        use crate::{wslog, Identity};
        // 창립자 워크스페이스.
        let founder = Identity::generate();
        let (genesis, wid) = wslog::build_genesis(&founder, "team", 1);
        let head = wslog::entry_hash(&wslog::entry_bytes(&genesis));

        // 발급 → 조인.
        let (code, _locator, blob) = make_invite(&founder, wid, head, [0x55; 32], 999_999).unwrap();
        let joiner = Identity::generate();
        let (add, secret) = build_add_from_blob(&code, wid, &blob, &joiner.public(), head, 1, 2).unwrap();
        assert_eq!(secret.server_fp, [0x55; 32]);

        // 체인 검증: 창립자 + 조인자.
        let chain = vec![wslog::entry_bytes(&genesis), wslog::entry_bytes(&add)];
        let v = wslog::verify_chain(&chain, 0).unwrap();
        assert_eq!(v.accepted, 2);
        assert!(v.is_member(&joiner.device_id()));
    }

    #[test]
    fn blob_seal_open_roundtrip() {
        let wid = [9u8; 32];
        let secret = InviteSecret {
            grant_sk_pkcs8: vec![1, 2, 3],
            grant_cert: GrantCert {
                grant_pk: vec![4, 5],
                sponsor: [1u8; 32],
                expires_at: 100,
                workspace_id: wid,
                sig: vec![6, 7],
            },
            workspace_id: wid,
            log_head_hash: [8u8; 32],
            server_fp: [7u8; 32],
        };
        let k_seal = [3u8; 32];
        let sealed = seal_blob(&k_seal, &secret).unwrap();
        let back = open_blob(&k_seal, &wid, &sealed).unwrap();
        assert_eq!(secret, back);
        // 잘못된 workspace_id(aad) → 실패.
        assert!(open_blob(&k_seal, &[0u8; 32], &sealed).is_err());
    }
}
