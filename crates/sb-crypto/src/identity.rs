//! 장치 신원 (§4.2). P-256 identity(=TLS cert + 로그 서명 겸용) + X25519 KEM 수신키.
//!
//! `device_id = SHA-256(P-256 SPKI DER)`. 로그/회전 서명은 항상 도메인 분리 컨텍스트를
//! **첫 바이트부터** 프리픽스한다(§4.2 D4 — TLS CertificateVerify와 구조적 비충돌).

use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use rand_core::OsRng;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroize;

use sb_proto::DeviceId;

use crate::hash::sha256;
use crate::CryptoError;

/// 로그 엔트리 서명 도메인.
pub const DOMAIN_LOG: &[u8] = b"sb/log/v1\x00";
/// grant 인증서 서명 도메인.
pub const DOMAIN_GRANT: &[u8] = b"sb/grant-v1\x00";
/// GK wrap(RotationBlob) 서명 도메인.
pub const DOMAIN_ROTWRAP: &[u8] = b"sb/rotwrap-v1\x00";

/// 도메인 분리된 서명 대상 바이트 = `domain || msg`.
fn signing_input(domain: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(domain.len() + msg.len());
    v.extend_from_slice(domain);
    v.extend_from_slice(msg);
    v
}

/// 임의 P-256 서명키로 도메인 분리 서명 → DER 인코딩 서명 바이트.
pub fn sign_with(sk: &SigningKey, domain: &[u8], msg: &[u8]) -> Vec<u8> {
    let sig: Signature = sk.sign(&signing_input(domain, msg));
    sig.to_der().as_bytes().to_vec()
}

/// SPKI DER 공개키로 도메인 분리 서명 검증.
pub fn verify_with_spki(spki_der: &[u8], domain: &[u8], msg: &[u8], sig_der: &[u8]) -> bool {
    let Ok(vk) = VerifyingKey::from_public_key_der(spki_der) else {
        return false;
    };
    let Ok(sig) = Signature::from_der(sig_der) else {
        return false;
    };
    vk.verify(&signing_input(domain, msg), &sig).is_ok()
}

/// 공개 신원 — 와이어/로그에 실리는 부분.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPublic {
    pub device_id: DeviceId,
    /// P-256 SPKI DER.
    pub spki_der: Vec<u8>,
    /// X25519 공개키.
    pub kem_pk: [u8; 32],
}

/// 비밀 신원. 키는 keychain에 보관하고 프로세스 메모리에서만 복원한다.
pub struct Identity {
    signing: SigningKey,
    kem: StaticSecret,
    spki_der: Vec<u8>,
    device_id: DeviceId,
}

impl Identity {
    /// 새 신원 생성 (P-256 + X25519).
    pub fn generate() -> Self {
        let signing = SigningKey::random(&mut OsRng);
        let kem = StaticSecret::random_from_rng(OsRng);
        Self::from_keys(signing, kem)
    }

    fn from_keys(signing: SigningKey, kem: StaticSecret) -> Self {
        let vk = VerifyingKey::from(&signing);
        let spki_der = vk.to_public_key_der().expect("p256 SPKI").as_bytes().to_vec();
        let device_id = sha256(&spki_der);
        Self {
            signing,
            kem,
            spki_der,
            device_id,
        }
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    pub fn kem_public(&self) -> [u8; 32] {
        XPublicKey::from(&self.kem).to_bytes()
    }

    pub fn public(&self) -> IdentityPublic {
        IdentityPublic {
            device_id: self.device_id,
            spki_der: self.spki_der.clone(),
            kem_pk: self.kem_public(),
        }
    }

    /// 도메인 분리 서명.
    pub fn sign(&self, domain: &[u8], msg: &[u8]) -> Vec<u8> {
        sign_with(&self.signing, domain, msg)
    }

    /// KEM 비밀키 참조 (wrap unwrap용). 외부 유출 금지.
    pub(crate) fn kem_secret(&self) -> &StaticSecret {
        &self.kem
    }

    // ── keychain 직렬화 ──

    /// 서명키 PKCS#8 DER (keychain 저장용). 사용 후 즉시 zeroize 할 것.
    pub fn signing_pkcs8_der(&self) -> Result<Vec<u8>, CryptoError> {
        Ok(self
            .signing
            .to_pkcs8_der()
            .map_err(|_| CryptoError::KeyEncoding)?
            .as_bytes()
            .to_vec())
    }

    /// KEM 비밀키 원바이트 (keychain 저장용).
    pub fn kem_secret_bytes(&self) -> [u8; 32] {
        self.kem.to_bytes()
    }

    /// keychain에서 복원.
    pub fn from_parts(signing_pkcs8_der: &[u8], kem_secret: &[u8; 32]) -> Result<Self, CryptoError> {
        let signing = SigningKey::from_pkcs8_der(signing_pkcs8_der).map_err(|_| CryptoError::KeyEncoding)?;
        let kem = StaticSecret::from(*kem_secret);
        Ok(Self::from_keys(signing, kem))
    }

    /// rustls용 자기서명 TLS cert + 개인키 DER 생성 (§4.5). sb-net/sb-server가 rustls 타입으로 감싼다.
    ///
    /// 반환 `(cert_der, key_pkcs8_der)`. cert의 키 = 이 identity의 P-256 키이므로
    /// 지문(SHA-256 SPKI)이 device_id와 일치한다.
    pub fn tls_material(&self) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        let pkcs8 = self
            .signing
            .to_pkcs8_der()
            .map_err(|_| CryptoError::KeyEncoding)?;
        let key_pair = rcgen::KeyPair::try_from(pkcs8.as_bytes()).map_err(|_| CryptoError::CertGen)?;
        let params = rcgen::CertificateParams::new(vec!["shareboard".to_string()])
            .map_err(|_| CryptoError::CertGen)?;
        let cert = params.self_signed(&key_pair).map_err(|_| CryptoError::CertGen)?;
        Ok((cert.der().to_vec(), pkcs8.as_bytes().to_vec()))
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        // SigningKey/StaticSecret 는 자체 zeroize-on-drop. spki/device_id 는 공개값이라 무해.
        self.spki_der.zeroize();
    }
}

/// SPKI DER → device_id.
pub fn device_id_from_spki(spki_der: &[u8]) -> DeviceId {
    sha256(spki_der)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_stable_across_restore() {
        let id = Identity::generate();
        let did = id.device_id();
        let pkcs8 = id.signing_pkcs8_der().unwrap();
        let kem = id.kem_secret_bytes();
        let restored = Identity::from_parts(&pkcs8, &kem).unwrap();
        assert_eq!(did, restored.device_id());
        assert_eq!(id.kem_public(), restored.kem_public());
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = Identity::generate();
        let sig = id.sign(DOMAIN_LOG, b"entry-bytes");
        assert!(verify_with_spki(id.spki_der(), DOMAIN_LOG, b"entry-bytes", &sig));
    }

    #[test]
    fn domain_separation_enforced() {
        let id = Identity::generate();
        let sig = id.sign(DOMAIN_LOG, b"msg");
        // 다른 도메인으로는 검증 실패해야 함 (교차 프로토콜 재사용 차단).
        assert!(!verify_with_spki(id.spki_der(), DOMAIN_GRANT, b"msg", &sig));
    }

    #[test]
    fn wrong_key_rejected() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = a.sign(DOMAIN_LOG, b"msg");
        assert!(!verify_with_spki(b.spki_der(), DOMAIN_LOG, b"msg", &sig));
    }

    #[test]
    fn tls_cert_fingerprint_matches_device_id() {
        let id = Identity::generate();
        let (cert_der, _key) = id.tls_material().unwrap();
        // cert에서 SPKI를 다시 뽑아 지문 비교하는 것은 sb-net 몫이지만,
        // 여기서는 cert가 비지 않고 device_id가 SPKI 기반임을 확인.
        assert!(!cert_der.is_empty());
        assert_eq!(id.device_id(), device_id_from_spki(id.spki_der()));
    }
}
