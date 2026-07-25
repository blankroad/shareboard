//! GK wrap — X25519 ECDH-ES + HKDF + XChaCha20-Poly1305 (§4.4, §4.1 축 B).
//!
//! 회전자/발급자가 새 기기의 정적 KEM 공개키로 GK(를 담은 `RotationBlob`)를 봉인한다.
//! 와이어 형식: `eph_pub(32) || nonce(24) || ciphertext`.

use hkdf::Hkdf;
use rand_core::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::aead::{xopen, xseal, NONCE_LEN};
use crate::hash::INFO_WRAP;
use crate::identity::Identity;
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
}
