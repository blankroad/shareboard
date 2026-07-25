//! # sb-net
//!
//! shareboard 클라이언트 네트워킹 (PLAN.md §4.5, §5).
//!
//! - [`tls`] — TLS 1.3 클라이언트 설정 + 서버 지문 pinning 검증기 + mTLS.
//! - [`conn`] — 프레이밍된 연결(`Connection`), `connect()`(주소 allowlist 포함).
//!
//! 연결 FSM/재연결 백오프/Hello 핸드셰이크 오케스트레이션은 상위(Tauri 앱)가 이 프리미티브로 구성한다.

pub mod conn;
pub mod tls;

pub use conn::{connect, framed_codec, spawn, ClientHandle, Connection};
pub use tls::{cert_fingerprint, client_config, init_crypto, PinnedServerVerifier};

/// 네트워킹 오류.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS 설정: {0}")]
    Tls(String),
    #[error("주소가 LAN allowlist 밖: {0}")]
    AddressNotAllowed(String),
    #[error("연결 종료됨")]
    Closed,
    #[error(transparent)]
    Proto(#[from] sb_proto::ProtoError),
}

#[cfg(test)]
mod integration {
    //! loopback 에서 실제 TLS 서버를 세우고 Envelope 왕복을 검증한다.
    use super::*;
    use std::sync::Arc;

    use futures::{SinkExt, StreamExt};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use tokio_util::codec::Framed;

    use sb_crypto::Identity;
    use sb_proto::{decode_env, encode_env, C2s, S2c, SignalHdr, Welcome};

    #[derive(Debug)]
    struct AcceptAnyClient;
    impl rustls::server::danger::ClientCertVerifier for AcceptAnyClient {
        fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
            &[]
        }
        fn verify_client_cert(
            &self,
            _e: &CertificateDer<'_>,
            _i: &[CertificateDer<'_>],
            _n: rustls::pki_types::UnixTime,
        ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _m: &[u8],
            _c: &CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                m,
                c,
                d,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    #[tokio::test]
    async fn hello_welcome_roundtrip_over_tls() {
        init_crypto();

        // 서버 신원 → 지문.
        let server_id = Identity::generate();
        let (scert_der, skey_der) = server_id.tls_material().unwrap();
        let scert = CertificateDer::from(scert_der);
        let server_fp = cert_fingerprint(&scert);
        let skey = PrivateKeyDer::try_from(skey_der).unwrap();

        let server_cfg = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(Arc::new(AcceptAnyClient))
            .with_single_cert(vec![scert], skey)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // 서버: Hello 를 받으면 Welcome, ClipSignal 을 받으면 SignalFanout 으로 되돌린다.
        let srv = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            // 클라 mTLS cert 지문 확인(존재만).
            let (_io, conn) = tls.get_ref();
            assert!(conn.peer_certificates().is_some(), "클라 cert 제시됨(mTLS)");
            let mut framed = Framed::new(tls, framed_codec());

            while let Some(Ok(frame)) = framed.next().await {
                let msg: C2s = decode_env(&frame).unwrap();
                match msg {
                    C2s::Hello(h) => {
                        let w = Welcome {
                            chosen_version: 2,
                            epoch: 0,
                            log_tail: vec![],
                            pending_key_update: None,
                            presence: vec![],
                            head: vec![],
                            server_time_ms: 0,
                        };
                        assert_eq!(h.proto_max, 2);
                        framed
                            .send(encode_env(S2c::Welcome(w)).unwrap().into())
                            .await
                            .unwrap();
                    }
                    C2s::ClipSignal { hdr, e2e } => {
                        let f = S2c::SignalFanout {
                            origin: [1u8; 32],
                            hdr,
                            e2e,
                        };
                        framed.send(encode_env(f).unwrap().into()).await.unwrap();
                    }
                    C2s::Bye { .. } => break,
                    _ => {}
                }
            }
        });

        // 클라이언트: 지문 pinning 으로 접속 → Hello/Welcome → ClipSignal/Fanout.
        let client_id = Identity::generate();
        let mut conn = connect(addr, server_fp, &client_id).await.unwrap();

        conn.send(C2s::Hello(sb_proto::Hello {
            device_id: client_id.device_id(),
            proto_min: 2,
            proto_max: 2,
            app_version: "test".into(),
            epoch: 0,
            log_head: (0, [0u8; 32]),
        }))
        .await
        .unwrap();
        match conn.recv().await.unwrap() {
            S2c::Welcome(w) => assert_eq!(w.chosen_version, 2),
            other => panic!("Welcome 기대: {other:?}"),
        }

        conn.send(C2s::ClipSignal {
            hdr: SignalHdr {
                id: [9u8; 32],
                epoch: 0,
                ct_size: 5,
            },
            e2e: vec![1, 2, 3],
        })
        .await
        .unwrap();
        match conn.recv().await.unwrap() {
            S2c::SignalFanout { hdr, e2e, .. } => {
                assert_eq!(hdr.id, [9u8; 32]);
                assert_eq!(e2e, vec![1, 2, 3]);
            }
            other => panic!("SignalFanout 기대: {other:?}"),
        }

        conn.send(C2s::Bye {
            reason: sb_proto::ByeReason::Shutdown,
        })
        .await
        .unwrap();
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_fingerprint_rejected() {
        init_crypto();
        let server_id = Identity::generate();
        let (scert_der, skey_der) = server_id.tls_material().unwrap();
        let scert = CertificateDer::from(scert_der);
        let skey = PrivateKeyDer::try_from(skey_der).unwrap();

        let server_cfg = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(Arc::new(AcceptAnyClient))
            .with_single_cert(vec![scert], skey)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                let _ = acceptor.accept(tcp).await; // 핸드셰이크 실패 예상
            }
        });

        // 잘못된 지문으로 접속 → 실패.
        let client_id = Identity::generate();
        let res = connect(addr, [0xff; 32], &client_id).await;
        assert!(res.is_err(), "지문 불일치 시 연결 실패");
    }
}
