//! E2E payload 평면 (§5.2 하단). 서버가 파싱할 수 없는 GK 암호문의 **내부** 구조.
//!
//! 이 타입들은 그룹 키(GK)로 봉인되기 전/후의 평문 표현이다. 서버는 이 바이트를
//! 절대 복호할 수 없으므로(blind relay), 여기 담긴 정렬키·kind·장치명은 서버에 불가시다.

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, Epoch};
use crate::kinds::{ContentKind, EpochReason};

/// `ClipSignal.e2e = seal(k_sig, SignalBody, aad = SignalHdr‖origin)` 의 평문 (§5.2).
///
/// 정렬키(lamport/wall_ts)와 kind, inline 여부까지 전부 여기 들어가 암호문 안에 숨는다(D31).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalBody {
    pub kind: ContentKind,
    /// 원문 크기(압축·암호화 전).
    pub plain_size: u64,
    /// LWW 1차 키 — 서버 비가시.
    pub lamport: u64,
    /// LWW 2차 키 + UI 표시.
    pub wall_ts_ms: u64,
    /// 서버 스탬프 origin과 대조(AAD로도 결합).
    pub origin: DeviceId,
    /// zstd 적용 여부(4KiB 초과 텍스트, 암호화 전 압축).
    pub compressed: bool,
    /// ≤32KiB 텍스트(압축 후) — 존재 여부 자체도 암호문 안에서 은닉됨.
    #[serde(default)]
    pub inline: Option<Vec<u8>>,
}

/// `KeyUpdate.wrap` 내부 — X25519 ECDH-ES + HKDF + XChaCha20-Poly1305 봉인 평문 (§5.2, §4.4).
///
/// AAD = `{workspace_id, epoch, log head 해시, epoch_entry_hash, member_set_hash, to, from}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationBlob {
    pub new_epoch: Epoch,
    /// 새 그룹 키(256-bit). 반드시 봉인된 상태로만 와이어에 오른다.
    pub group_key: [u8; 32],
    pub reason: EpochReason,
    /// 회전: 체인에 채택된 Epoch(e+1) 엔트리 해시 / Join: 현 log head 해시.
    pub epoch_entry_hash: [u8; 32],
    /// 수신자가 검증한 `Epoch.member_set_hash` 와 일치할 때만 GK 채택(§4.4 step 2).
    pub member_set_hash: [u8; 32],
    pub from: DeviceId,
    /// 작성자 identity 서명(domain-sep) — 로그 멤버 자격 검증.
    pub sig: Vec<u8>,
}

/// enc_profile 평면 — `SetProfile.e2e = seal(GK, Profile)` (§5.2 SetProfile).
///
/// 단조 seq/epoch/ts를 포함해 서버의 stale profile 재생을 거부한다(§4.3.1 롤백 방어).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub platform: crate::kinds::Platform,
    /// 자신이 검증한 로그 head 해시 — 멤버 간 교차검증(§4.3.1 필수).
    pub log_head_hash: [u8; 32],
    /// 단조 seq — 후퇴 시 수신 폐기(replay 방어).
    pub seq: u64,
    pub epoch: Epoch,
    pub ts_ms: u64,
}
