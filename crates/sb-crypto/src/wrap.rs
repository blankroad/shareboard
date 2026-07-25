//! GK wrap — X25519 ECDH-ES + HKDF + XChaCha20-Poly1305 (§4.4, §4.1 축 B).
//!
//! 회전자/발급자가 새 기기의 정적 KEM 공개키로 GK(를 담은 `RotationBlob`)를 봉인한다.
//! 와이어 형식: `eph_pub(32) || nonce(24) || ciphertext`.

use hkdf::Hkdf;
use rand_core::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroize;

use sb_proto::{DeviceId, Epoch, EpochReason, RotationBlob};

use crate::aead::{xopen, xseal, NONCE_LEN};
use crate::hash::INFO_WRAP;
use crate::identity::{verify_with_spki, Identity, DOMAIN_ROTWRAP};
use crate::wslog::VerifiedLog;
use crate::CryptoError;

const EPH_LEN: usize = 32;

fn kdf(shared: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(INFO_WRAP, &mut key).expect("32B HKDF");
    key
}

/// 수신자 KEM 공개키로 `plaintext`(=인코딩된 RotationBlob)를 봉인.
///
/// `aad` = `{workspace_id, epoch, log head, epoch_entry_hash, member_set_hash, to, from}` 캐노니컬 바이트.
pub fn wrap(recipient_kem_pk: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = XPublicKey::from(&eph);
    let recip = XPublicKey::from(*recipient_kem_pk);
    let mut shared = eph.diffie_hellman(&recip).to_bytes();
    let mut key = kdf(&shared);
    shared.zeroize();

    let sealed = xseal(&key, aad, plaintext);
    key.zeroize();
    let sealed = sealed?;

    let mut out = Vec::with_capacity(EPH_LEN + sealed.len());
    out.extend_from_slice(eph_pub.as_bytes());
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// 수신자 identity의 KEM 비밀키로 unwrap → 평문(RotationBlob 바이트).
pub fn unwrap(recipient: &Identity, aad: &[u8], wrapped: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if wrapped.len() < EPH_LEN + NONCE_LEN + 16 {
        return Err(CryptoError::Open);
    }
    let (eph_bytes, sealed) = wrapped.split_at(EPH_LEN);
    let mut eph_arr = [0u8; 32];
    eph_arr.copy_from_slice(eph_bytes);
    let eph_pub = XPublicKey::from(eph_arr);

    let mut shared = recipient.kem_secret().diffie_hellman(&eph_pub).to_bytes();
    let mut key = kdf(&shared);
    shared.zeroize();
    let r = xopen(&key, aad, sealed);
    key.zeroize();
    r
}

// ───────────── RotationBlob 고수준 API (서명 + AAD 바인딩, §4.4) ─────────────

/// 회전 wrap AAD = workspace_id ‖ 수신자 device_id. ECDH 로 이미 수신자 한정되나 이중 결속.
/// epoch 는 AAD 에 넣지 않고(수신자가 KeyUpdatePush 만으로는 모름) 서명·post-decrypt 검증으로 결속.
pub fn rotation_aad(workspace_id: &[u8; 32], to: &DeviceId) -> Vec<u8> {
    let mut a = Vec::with_capacity(64);
    a.extend_from_slice(workspace_id);
    a.extend_from_slice(to);
    a
}

/// sig 필드를 비운 서명 뷰 바이트.
fn rotation_sign_view(blob: &RotationBlob) -> Vec<u8> {
    let mut c = blob.clone();
    c.sig.clear();
    let mut b = Vec::new();
    ciborium::into_writer(&c, &mut b).expect("RotationBlob CBOR encode");
    b
}

/// 서명된 RotationBlob 생성 — `from` 이 자기 identity 로 서명(DOMAIN_ROTWRAP).
#[allow(clippy::too_many_arguments)]
pub fn build_signed_rotation(
    from: &Identity,
    new_epoch: Epoch,
    group_key: [u8; 32],
    reason: EpochReason,
    epoch_entry_hash: [u8; 32],
    member_set_hash: [u8; 32],
) -> RotationBlob {
    let mut blob = RotationBlob {
        new_epoch,
        group_key,
        reason,
        epoch_entry_hash,
        member_set_hash,
        from: from.device_id(),
        sig: Vec::new(),
    };
    blob.sig = from.sign(DOMAIN_ROTWRAP, &rotation_sign_view(&blob));
    blob
}

/// 수신자 KEM pk 로 서명된 RotationBlob 봉인.
pub fn seal_rotation(
    recipient_kem_pk: &[u8; 32],
    workspace_id: &[u8; 32],
    to: &DeviceId,
    blob: &RotationBlob,
) -> Result<Vec<u8>, CryptoError> {
    let mut bytes = Vec::new();
    ciborium::into_writer(blob, &mut bytes).map_err(|_| CryptoError::Encode)?;
    wrap(recipient_kem_pk, &rotation_aad(workspace_id, to), &bytes)
}

/// 수신자 identity 로 unwrap → RotationBlob (아직 검증 전).
pub fn open_rotation(
    recipient: &Identity,
    workspace_id: &[u8; 32],
    wrapped: &[u8],
) -> Result<RotationBlob, CryptoError> {
    let aad = rotation_aad(workspace_id, &recipient.device_id());
    let bytes = unwrap(recipient, &aad, wrapped)?;
    ciborium::from_reader(&bytes[..]).map_err(|_| CryptoError::Decode)
}

/// 복호된 RotationBlob 를 검증된 로그에 대해 검증 (§4.3.1 규칙 ⑥·§4.4 step 2).
///
/// (1) `from` 이 현 roster 멤버 (2) 서명 유효 (3) new_epoch == 수신자 현 epoch
/// (4) member_set_hash == 수신자 roster 해시(roster 밖 키 wrap 탐지).
pub fn verify_rotation(blob: &RotationBlob, log: &VerifiedLog) -> bool {
    let Some(member) = log.members.get(&blob.from) else {
        return false;
    };
    if !verify_with_spki(
        &member.spki_der,
        DOMAIN_ROTWRAP,
        &rotation_sign_view(blob),
        &blob.sig,
    ) {
        return false;
    }
    if blob.new_epoch != log.epoch {
        return false;
    }
    if blob.member_set_hash != log.member_set_hash() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_roundtrip() {
        let recipient = Identity::generate();
        let wrapped = wrap(&recipient.kem_public(), b"aad", b"group-key-blob").unwrap();
        let pt = unwrap(&recipient, b"aad", &wrapped).unwrap();
        assert_eq!(pt, b"group-key-blob");
    }

    #[test]
    fn wrong_recipient_cannot_open() {
        let a = Identity::generate();
        let b = Identity::generate();
        let wrapped = wrap(&a.kem_public(), b"aad", b"secret").unwrap();
        assert!(unwrap(&b, b"aad", &wrapped).is_err());
    }

    #[test]
    fn aad_binding_enforced() {
        // AAD에 epoch/수신자를 묶으므로 서버의 교차 epoch reflection 차단(§4.4).
        let r = Identity::generate();
        let wrapped = wrap(&r.kem_public(), b"epoch=5", b"gk").unwrap();
        assert!(unwrap(&r, b"epoch=6", &wrapped).is_err());
    }

    // ── RotationBlob 고수준 API 테스트 ──
    use crate::wslog;
    use sb_proto::EpochReason;

    /// 창립자 + 조인자 2인 워크스페이스 로그 + 로그 상태 반환.
    fn two_member_log() -> (Identity, Identity, wslog::VerifiedLog) {
        let founder = Identity::generate();
        let (genesis, wid) = wslog::build_genesis(&founder, "team", 1);
        let head = wslog::entry_hash(&wslog::entry_bytes(&genesis));
        let grant = crate::generate_grant();
        let gc = wslog::build_grant_cert(&founder, grant.pk_der, 10_000, wid);
        let joiner = Identity::generate();
        let add = wslog::build_add(&grant.sk, gc, &joiner.public(), head, 1, 2);
        let chain = vec![wslog::entry_bytes(&genesis), wslog::entry_bytes(&add)];
        let v = wslog::verify_chain(&chain, 0).unwrap();
        (founder, joiner, v)
    }

    #[test]
    fn signed_rotation_roundtrip_and_verify() {
        let (founder, joiner, log) = two_member_log();
        let wid = log.workspace_id;
        let gk = [0x33u8; 32];
        // 창립자가 GK 를 조인자에게 wrap.
        let blob = build_signed_rotation(
            &founder,
            log.epoch,
            gk,
            EpochReason::Join,
            log.head_hash,
            log.member_set_hash(),
        );
        let joiner_pub = joiner.public();
        let wrapped = seal_rotation(&joiner_pub.kem_pk, &wid, &joiner.device_id(), &blob).unwrap();

        // 조인자가 열고 검증.
        let opened = open_rotation(&joiner, &wid, &wrapped).unwrap();
        assert_eq!(opened.group_key, gk);
        assert!(verify_rotation(&opened, &log), "정상 wrap 은 검증 통과");
    }

    #[test]
    fn non_member_wrap_rejected() {
        let (_founder, joiner, log) = two_member_log();
        // 로그에 없는 외부인이 서명한 wrap → verify 실패(roster 밖).
        let outsider = Identity::generate();
        let blob = build_signed_rotation(
            &outsider,
            log.epoch,
            [1u8; 32],
            EpochReason::Join,
            log.head_hash,
            log.member_set_hash(),
        );
        assert!(
            !verify_rotation(&blob, &log),
            "비멤버 서명 wrap 거부(서버 키 주입 차단)"
        );
    }

    #[test]
    fn tampered_signature_rejected() {
        let (founder, _joiner, log) = two_member_log();
        let mut blob = build_signed_rotation(
            &founder,
            log.epoch,
            [2u8; 32],
            EpochReason::Join,
            log.head_hash,
            log.member_set_hash(),
        );
        // group_key 를 바꿔치기 → 서명 불일치.
        blob.group_key = [0xFFu8; 32];
        assert!(!verify_rotation(&blob, &log));
    }

    #[test]
    fn wrong_member_set_hash_rejected() {
        let (founder, _joiner, log) = two_member_log();
        let blob = build_signed_rotation(
            &founder,
            log.epoch,
            [2u8; 32],
            EpochReason::Join,
            log.head_hash,
            [0xAAu8; 32], // 잘못된 roster 해시(규칙 ⑥ 위반)
        );
        assert!(!verify_rotation(&blob, &log));
    }
}
