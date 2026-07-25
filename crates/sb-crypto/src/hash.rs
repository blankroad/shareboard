//! 해시와 키 파생 (§4.6). BLAKE3(keyed/plain), HKDF-SHA256 GK 서브키, workspace_id.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use sb_proto::ContentId;

/// GK 서브키 도메인 (§4.6) — 용도별 분리로 블롭 교차 스플라이싱 차단.
pub const INFO_CID: &[u8] = b"sb/cid-v1";
pub const INFO_SIG: &[u8] = b"sb/sig-v1";
pub const INFO_BODY: &[u8] = b"sb/body-v1";

/// 초대 파생 도메인 (§4.3.3).
pub const INFO_INV_LOCATOR: &[u8] = b"sb/inv-locator";
pub const INFO_INV_SEAL: &[u8] = b"sb/inv-seal";

/// GK wrap 도메인 (§4.4).
pub const INFO_WRAP: &[u8] = b"sb/wrap-v1";

/// 평문 BLAKE3.
pub fn blake3_hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// keyed BLAKE3 — 저엔트로피 원문의 사전 대입 역산 차단(D21).
pub fn keyed_blake3(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(key, data).as_bytes()
}

/// SHA-256 (device_id 파생, TLS 지문 등).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    Sha256::digest(data).into()
}

/// HMAC-SHA256 — workspace.json 무결성 MAC(D22), key confirmation 등.
pub fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 는 임의 키 길이 허용");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// `workspace_id = BLAKE3(Genesis 바이트열)` (§4.3.1).
pub fn workspace_id(genesis_bytes: &[u8]) -> [u8; 32] {
    blake3_hash(genesis_bytes)
}

/// HKDF-SHA256(ikm=GK, info) → 32B 서브키.
pub fn derive_subkey(gk: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, gk);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .expect("32 bytes is valid HKDF-SHA256 length");
    out
}

/// ContentId = keyed BLAKE3(k_cid, plaintext) (§5.2, D21).
pub fn content_id(k_cid: &[u8; 32], plaintext: &[u8]) -> ContentId {
    keyed_blake3(k_cid, plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subkeys_domain_separated() {
        let gk = [42u8; 32];
        let a = derive_subkey(&gk, INFO_CID);
        let b = derive_subkey(&gk, INFO_SIG);
        let c = derive_subkey(&gk, INFO_BODY);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn keyed_hash_depends_on_key() {
        let a = keyed_blake3(&[1u8; 32], b"pw");
        let b = keyed_blake3(&[2u8; 32], b"pw");
        assert_ne!(a, b, "다른 키면 같은 원문도 다른 해시");
    }

    #[test]
    fn content_id_stable() {
        let k = [9u8; 32];
        assert_eq!(content_id(&k, b"hello"), content_id(&k, b"hello"));
        assert_ne!(content_id(&k, b"hello"), content_id(&k, b"world"));
    }
}
