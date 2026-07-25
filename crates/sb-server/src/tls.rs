//! 서버 TLS 설정 (§4.5). 자기서명 cert, TLS 1.3 전용, 클라 cert 는 모두 수용하고
//! lane(Member/Guest) 은 핸드셰이크 후 지문으로 판정한다.

use std::sync::Arc;

use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};

use sb_crypto::Identity;

/// 클라 cert 를 모두 수용(lane 판정은 핸드셰이크 후). TLS 1.3 서명은 실제 검증(키 소유 증명).
#[derive(Debug)]
pub struct AcceptAnyClient {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl AcceptAnyClient {
    pub fn new() -> Self {
        Self {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl Default for AcceptAnyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientCertVerifier for AcceptAnyClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _end: &CertificateDer<'_>,
        _int: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &CertificateDer<'_>,
        _d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ED25519,
            SignatureScheme::ECDSA_NISTP384_SHA384,
        ]
    }
}

/// 서버 TLS 설정.
pub fn server_config(identity: &Identity) -> anyhow::Result<Arc<ServerConfig>> {
    let (cert_der, key_der) = identity.tls_material().map_err(|e| anyhow::anyhow!("{e}"))?;
    let cert = CertificateDer::from(cert_der);
    let key = PrivateKeyDer::try_from(key_der).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cfg = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_client_cert_verifier(Arc::new(AcceptAnyClient::new()))
        .with_single_cert(vec![cert], key)?;
    Ok(Arc::new(cfg))
}
