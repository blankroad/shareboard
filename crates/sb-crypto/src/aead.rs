//! XChaCha20-Poly1305 봉인 헬퍼 (§4.6). 24B 랜덤 nonce를 암호문 앞에 전치한다.
//!
//! GK 콘텐츠 봉인, 초대 blob 봉인, GK wrap 등 모든 AEAD 사용처의 공통 프리미티브.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};

use crate::CryptoError;

pub const NONCE_LEN: usize = sb_proto::params::NONCE_LEN; // 24

/// `key` 로 `pt` 를 봉인. 반환 = `nonce(24) || ciphertext`. `aad` 는 무결성에 결합(암호화 안 됨).
pub fn xseal(key: &[u8; 32], aad: &[u8], pt: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: pt, aad })
        .map_err(|_| CryptoError::Seal)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// `xseal` 의 역. `sealed = nonce(24) || ciphertext`.
pub fn xopen(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LEN + 16 {
        return Err(CryptoError::Open);
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = [7u8; 32];
        let sealed = xseal(&key, b"aad", b"secret content").unwrap();
        let pt = xopen(&key, b"aad", &sealed).unwrap();
        assert_eq!(pt, b"secret content");
    }

    #[test]
    fn wrong_aad_fails() {
        let key = [7u8; 32];
        let sealed = xseal(&key, b"aad", b"x").unwrap();
        assert!(xopen(&key, b"different", &sealed).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = xseal(&[1u8; 32], b"", b"x").unwrap();
        assert!(xopen(&[2u8; 32], b"", &sealed).is_err());
    }

    #[test]
    fn nonces_differ() {
        let key = [3u8; 32];
        let a = xseal(&key, b"", b"same").unwrap();
        let b = xseal(&key, b"", b"same").unwrap();
        assert_ne!(a[..NONCE_LEN], b[..NONCE_LEN], "매 봉인마다 랜덤 nonce");
    }
}
