//! # sb-core
//!
//! shareboard 동기화 엔진과 도메인 로직 (PLAN.md §3, §5, §7).
//!
//! - [`engine::SyncEngine`] — 로컬 복사 발행 / 원격 신호 LWW 판정 / 콘텐츠 적용 오케스트레이션.
//! - [`lww`] — LWW 순서 판정 + Lamport 시계 (§5.3).
//! - [`suppress`] — 에코 방지 suppress set + recent LRU.
//! - [`history`] — 인메모리 히스토리 모델.
//! - [`settings`] — settings.json 스키마와 기본값 (§7.3).
//! - [`compress`] — zstd(암호화 전) 압축.
//!
//! OS 클립보드·네트워크·저장은 각각 sb-clipboard/sb-net/sb-store 가 담당한다.

pub mod compress;
pub mod engine;
pub mod history;
pub mod lww;
pub mod settings;
pub mod suppress;

pub use engine::{AppliedContent, IgnoreReason, LocalOutcome, OutgoingSignal, RemoteDecision, SyncEngine};
pub use history::{HistoryBuffer, HistoryItem, Origin};
pub use lww::LwwState;
pub use settings::{EngineConfig, Settings};
pub use suppress::SuppressSet;

/// sb-core 오류.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Crypto(#[from] sb_crypto::CryptoError),
    #[error(transparent)]
    Proto(#[from] sb_proto::ProtoError),
    #[error("zstd 압축 해제 실패")]
    Decompress,
    #[error("ContentId 불일치(무결성 위반)")]
    CidMismatch,
    #[error("대기 중인 fetch 없음")]
    NoPending,
    #[error("설정 직렬화 실패")]
    Settings,
}
