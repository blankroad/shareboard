//! 워크스페이스 로그 엔트리 (§4.3.1 / §5.2) — roster 무결성 서명 해시체인.
//!
//! 엔트리는 **저장 바이트열 그대로** 보존·전파하고 서명도 그 바이트열에 수행한다
//! (재직렬화/canonicalization 문제 원천 회피 — §4.3.1). 따라서 검증 계층(sb-crypto)은
//! 이 타입을 디코드해 필드를 읽되, 서명 검증은 원본 바이트에 대해 수행해야 한다.

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, Epoch};
use crate::kinds::EpochReason;

/// grant 인증서 — sponsor 장치키로 서명 (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantCert {
    /// 초대별 일회용 grant 공개키(P-256 SPKI DER).
    pub grant_pk: Vec<u8>,
    pub sponsor: DeviceId,
    pub expires_at: u64,
    pub workspace_id: [u8; 32],
    /// sponsor 장치키 서명(domain-sep).
    pub sig: Vec<u8>,
}

/// 워크스페이스 로그 엔트리 (§5.2). 각 variant는 §4.3.1의 서명 주체가 다름.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogEntry {
    /// 생성자 자기 서명. `workspace_id = BLAKE3(Genesis 바이트열)`.
    Genesis {
        v: u16,
        workspace_name: String,
        /// 생성자 identity 공개키(P-256 SPKI DER).
        creator_spki: Vec<u8>,
        creator_kem_pk: [u8; 32],
        created_at: u64,
        sig: Vec<u8>,
    },
    /// grant_sk 서명. 새 멤버 조인.
    Add {
        prev_hash: [u8; 32],
        seq: u64,
        grant_cert: GrantCert,
        subject_spki: Vec<u8>,
        subject_kem_pk: [u8; 32],
        joined_at: u64,
        sig: Vec<u8>,
    },
    /// 잔존 멤버 장치키 서명. 멤버 제거.
    Remove {
        prev_hash: [u8; 32],
        seq: u64,
        target: DeviceId,
        ts: u64,
        by: DeviceId,
        sig: Vec<u8>,
    },
    /// 회전자 장치키 서명. epoch 회전.
    Epoch {
        prev_hash: [u8; 32],
        seq: u64,
        epoch_no: Epoch,
        rotator: DeviceId,
        reason: EpochReason,
        member_set_hash: [u8; 32],
        wrapped_bundle_hash: [u8; 32],
        ts: u64,
        sig: Vec<u8>,
    },
    /// 본인 서명. KEM 수신키 회전(§4.1 축 B 완화).
    RotateKem {
        prev_hash: [u8; 32],
        seq: u64,
        subject: DeviceId,
        new_kem_pk: [u8; 32],
        ts: u64,
        sig: Vec<u8>,
    },
}

impl LogEntry {
    /// 이 엔트리의 seq (Genesis는 0으로 취급).
    pub fn seq(&self) -> u64 {
        match self {
            LogEntry::Genesis { .. } => 0,
            LogEntry::Add { seq, .. }
            | LogEntry::Remove { seq, .. }
            | LogEntry::Epoch { seq, .. }
            | LogEntry::RotateKem { seq, .. } => *seq,
        }
    }

    /// 직전 엔트리 해시 (Genesis는 없음).
    pub fn prev_hash(&self) -> Option<&[u8; 32]> {
        match self {
            LogEntry::Genesis { .. } => None,
            LogEntry::Add { prev_hash, .. }
            | LogEntry::Remove { prev_hash, .. }
            | LogEntry::Epoch { prev_hash, .. }
            | LogEntry::RotateKem { prev_hash, .. } => Some(prev_hash),
        }
    }

    pub fn variant_name(&self) -> &'static str {
        match self {
            LogEntry::Genesis { .. } => "Genesis",
            LogEntry::Add { .. } => "Add",
            LogEntry::Remove { .. } => "Remove",
            LogEntry::Epoch { .. } => "Epoch",
            LogEntry::RotateKem { .. } => "RotateKem",
        }
    }
}
