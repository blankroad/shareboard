//! 열거형: 콘텐츠 종류, epoch 사유, 각종 거부/중단 사유 (§5.2).

use serde::{Deserialize, Serialize};

use crate::ids::DeviceId;

/// 동기화 대상 콘텐츠 종류.
///
/// `Files` 는 OS 클립보드의 파일 목록(Finder/탐색기 복사)이며, 콘텐츠 바이트는
/// [`crate::files::FileBundle`] 인코딩이다 — 파일 **이름까지 GK 암호문 안**에 들어가므로
/// 서버는 이름도 개수도 보지 못한다. proto 3 에서 추가됐다(구 버전 클라는 복호 실패 → 무시).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    Text,
    ImagePng,
    Files,
}

/// 장치 플랫폼 (enc_profile 표시용).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

/// epoch 회전 사유 (§4.4). `Revoke`는 제거 대상 device를 실어 grace 0 판정에 사용.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpochReason {
    /// 멤버 제거로 인한 회전 — grace 0 (제거 멤버의 구 키 LWW 주입 창 차단).
    Revoke(DeviceId),
    /// 수동 "키 회전" 버튼 — grace 10분.
    Manual,
    /// 주기 회전(옵션) — grace 10분.
    Periodic,
    /// 조인 시 현재 epoch GK 전달 (회전 아님).
    Join,
}

impl EpochReason {
    /// 이 사유의 epoch 전환 grace(초). revoke만 0, 나머지는 10분 (§4.4, §5.6).
    pub fn grace_secs(&self) -> u64 {
        match self {
            EpochReason::Revoke(_) => crate::params::EPOCH_GRACE_REVOKE_S,
            _ => crate::params::EPOCH_GRACE_MANUAL_S,
        }
    }
}

/// 콘텐츠 전송 중단 사유 (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortReason {
    /// 더 높은 LWW key signal 도착 — 현재 전송 폐기.
    Superseded,
    Cancelled,
    InternalError,
}

/// 콘텐츠 요청 거절 사유 (§5.2). C2s(origin의 Pull 거절)와 S2c 공용 superset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// 송신 캐시 만료/삭제로 원본 없음.
    Gone,
    TooLarge,
    Busy,
    /// (S2c) 보유자 전원 오프라인.
    OriginOffline,
    /// 해당 kind 동기화 비활성.
    KindDisabled,
    /// 수신 측 동기화 off.
    SyncDisabled,
}

/// 로그 append 거부 사유 (§4.5, §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppendRejectReason {
    /// admission 게이트 위반(초대 미소비 locator 바인딩 실패 등).
    NotAuthorized,
    /// head 경합 — tail 재취득 후 마지막 유효 head 위에 재append.
    Conflict,
    /// 서명/prev_hash 형식 사전 검증 실패 (명백한 위조).
    Malformed,
}

/// 세션 종료 사유 (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByeReason {
    Shutdown,
    /// (S2c) 서버가 제거 판단 — 클라는 "힌트"로만 처리(§5.4).
    Revoked,
    ProtocolError,
}

/// S2c `Error` 코드 (§5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    RateLimited,
    NotMember,
    VersionIncompatible,
    Busy,
    InviteFull,
    ProtocolError,
    InternalError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoke_grace_is_zero() {
        assert_eq!(EpochReason::Revoke([0u8; 32]).grace_secs(), 0);
        assert_eq!(EpochReason::Manual.grace_secs(), 600);
        assert_eq!(EpochReason::Periodic.grace_secs(), 600);
        assert_eq!(EpochReason::Join.grace_secs(), 600);
    }
}
