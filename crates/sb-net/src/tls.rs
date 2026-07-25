//! TLS 1.3 설정과 지문 pinning 검증기 (§4.5).
//!
//! 서버 인증서는 체인/유효기간/호스트명을 검증하지 않고 **SHA-256(전체 cert DER) 단일 비교**만
//! 수행한다(초대 blob 경유로 확정된 지문). 클라이언트는 mTLS 로 자기 장치 cert 를 제시한다.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use sb_crypto::Identity;

use crate::NetError;

/// cert 의 **SPKI(SubjectPublicKeyInfo)** SHA-256 지문 = `device_id` (§4.2).
///
/// 자기서명 ECDSA cert 의 DER 은 서명 난수 때문에 재생성마다 달라지므로 전체 DER 이 아니라
/// 안정적인 공개키(SPKI)를 해시한다. 파싱 실패 시 전체 DER 폴백(→ 불일치로 안전하게 거부됨).
pub fn cert_fingerprint(cert: &CertificateDer<'_>) -> [u8; 32] {
    match x509_parser::parse_x509_certificate(cert.as_ref()) {
        Ok((_, parsed)) => Sha256::digest(parsed.tbs_certificate.subject_pki.raw).into(),
        Err(_) => Sha256::digest(cert.as_ref()).into(),
    }
}

/// 지문 pinning 서버 검증기 — 지문 일치 외 X.509 체인/유효기간/호스트명 검증은 생략하되,
/// TLS 1.3 핸드셰이크 서명은 실제 검증한다(pinning=신원, 서명=키 소유 증명).
#[derive(Debug)]
pub struct PinnedServerVerifier {
    fingerprint: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl PinnedServerVerifier {
    pub fn new(fingerprint: [u8; 32]) -> Self {
        Self {
            fingerprint,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if cert_fingerprint(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("서버 지문 불일치(pinning)".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &CertificateDer<'_>,
        _d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // TLS 1.3 전용이므로 도달하지 않음. 안전하게 assertion.
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // 지문이 일치하는 자기서명 cert 의 키로 핸드셰이크 서명을 실제 검증(키 소유 증명).
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ED25519,
            SignatureScheme::ECDSA_NISTP384_SHA384,
        ]
    }
}

/// 프로세스 기본 암호 provider(ring) 설치. 앱/서버 시작 시 1회 호출.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 클라이언트 TLS 설정 — TLS 1.3 전용, 서버 지문 pinning + mTLS 장치 cert.
pub fn client_config(identity: &Identity, server_fp: [u8; 32]) -> Result<Arc<ClientConfig>, NetError> {
    let (cert_der, key_der) = identity
        .tls_material()
        .map_err(|_| NetError::Tls("cert 생성".into()))?;
    let cert = CertificateDer::from(cert_der);
    let key = PrivateKeyDer::try_from(key_der).map_err(|_| NetError::Tls("키 파싱".into()))?;

    let cfg = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier::new(server_fp)))
        .with_client_auth_cert(vec![cert], key)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    Ok(Arc::new(cfg))
}

/// 임의 이름 — pinning 이므로 호스트명 무의미. 서버에 보낼 SNI 자리.
pub fn dummy_server_name() -> ServerName<'static> {
    ServerName::try_from("shareboard").expect("valid dns name")
}
