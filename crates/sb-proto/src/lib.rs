//! # sb-proto
//!
//! shareboard 공유 프로토콜 타입. 클라이언트(sb-net/sb-core)와 서버(sb-server)가
//! 공통으로 참조하는 **와이어 메시지**, **E2E payload 평면**, **워크스페이스 로그 엔트리**,
//! **LAN allowlist**, **프로토콜 상수**를 정의한다.
//!
//! 설계 근거는 저장소 루트 `PLAN.md` (v2.0):
//! - §5.2 메시지 정의 → [`wire`], [`e2e`], [`log`]
//! - §5.3 LWW 판정 → [`ids::LwwKey`]
//! - §4.5 네트워크 격리 → [`net::is_lan_allowed`]
//! - §5.6 파라미터 표 → [`params`]
//!
//! 이 크레이트는 순수 데이터·상수만 담는다. 암호 연산은 sb-crypto, I/O는 sb-net/sb-server 소관.

pub mod e2e;
pub mod files;
pub mod ids;
pub mod kinds;
pub mod log;
pub mod net;
pub mod params;
pub mod wire;

pub use e2e::{Profile, RotationBlob, SignalBody};
pub use files::{safe_file_name, FileBundle, FileEntry};
pub use ids::{hex32, short_id, ContentId, DeviceId, Epoch, Lamport, Locator, LwwKey};
pub use kinds::{
    AbortReason, AppendRejectReason, ByeReason, ContentKind, EpochReason, ErrorCode, Platform, RejectReason,
};
pub use log::{GrantCert, LogEntry};
pub use net::is_lan_allowed;
pub use wire::{
    decode, decode_env, encode, encode_env, C2s, Envelope, Hello, KeyUpdate, PresenceEntry, S2c, SignalHdr,
    Welcome,
};

/// 프로토콜 인코딩/디코딩 오류.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("CBOR 인코딩 실패: {0}")]
    Encode(String),
    #[error("CBOR 디코딩 실패: {0}")]
    Decode(String),
    #[error(
        "지원하지 않는 프로토콜 버전: {got} (지원: {}..={})",
        params::PROTO_MIN,
        params::PROTO_MAX
    )]
    Version { got: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Envelope, SignalHdr};

    #[test]
    fn clipsignal_roundtrip() {
        let msg = C2s::ClipSignal {
            hdr: SignalHdr {
                id: [7u8; 32],
                epoch: 3,
                ct_size: 1234,
            },
            e2e: vec![1, 2, 3, 4, 5],
        };
        let bytes = encode_env(msg.clone()).unwrap();
        let back: C2s = decode_env(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn welcome_roundtrip() {
        let w = Welcome {
            chosen_version: 2,
            epoch: 1,
            log_tail: vec![vec![9, 9], vec![8]],
            pending_key_update: Some(vec![1, 2]),
            presence: vec![
                PresenceEntry {
                    device_id: [1u8; 32],
                    online: true,
                    addr: Some("192.168.0.5:1".into()),
                    enc_profile: None,
                },
                PresenceEntry {
                    device_id: [2u8; 32],
                    online: false,
                    addr: None,
                    enc_profile: Some(vec![5, 6]),
                },
            ],
            head: vec![(
                [1u8; 32],
                SignalHdr {
                    id: [0u8; 32],
                    epoch: 1,
                    ct_size: 10,
                },
                vec![1],
            )],
            server_time_ms: 42,
        };
        let bytes = encode_env(S2c::Welcome(w.clone())).unwrap();
        let back: S2c = decode_env(&bytes).unwrap();
        assert_eq!(S2c::Welcome(w), back);
    }

    #[test]
    fn signalbody_e2e_roundtrip() {
        let body = SignalBody {
            kind: ContentKind::Text,
            plain_size: 11,
            lamport: 7,
            wall_ts_ms: 1_700_000_000_000,
            origin: [3u8; 32],
            compressed: false,
            inline: Some(b"hello world".to_vec()),
        };
        let bytes = encode(&body).unwrap();
        let back: SignalBody = decode(&bytes).unwrap();
        assert_eq!(body, back);
    }

    #[test]
    fn logentry_roundtrip() {
        let e = LogEntry::Add {
            prev_hash: [1u8; 32],
            seq: 4,
            grant_cert: GrantCert {
                grant_pk: vec![0xde, 0xad],
                sponsor: [2u8; 32],
                expires_at: 999,
                workspace_id: [5u8; 32],
                sig: vec![0xbe, 0xef],
            },
            subject_spki: vec![1, 2, 3],
            subject_kem_pk: [6u8; 32],
            joined_at: 123,
            sig: vec![9, 9, 9],
        };
        let bytes = encode(&e).unwrap();
        let back: LogEntry = decode(&bytes).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn rejects_wrong_version() {
        // v=1 봉투를 강제로 만들어 디코딩 → Version 오류.
        let env = Envelope {
            v: 1u16,
            msg: C2s::Ping { nonce: 5 },
        };
        let bytes = encode(&env).unwrap();
        let err = decode_env::<C2s>(&bytes).unwrap_err();
        assert!(matches!(err, ProtoError::Version { got: 1 }));
    }

    #[test]
    fn epoch_reason_revoke_carries_device() {
        let r = EpochReason::Revoke([9u8; 32]);
        let bytes = encode(&r).unwrap();
        let back: EpochReason = decode(&bytes).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.grace_secs(), 0);
    }
}
