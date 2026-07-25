//! # sb-store
//!
//! shareboard 로컬 저장 (PLAN.md §4.2, §4.6, §7).
//!
//! - [`keystore`] — 키 저장소 추상화(Memory/File, OS keychain 은 상위에서 주입) + 폴백 사다리.
//! - [`keys`] — KeyManager: identity·history·dedup·ws-mac 키 get-or-create, crypto-erase.
//! - [`history`] — 암호화 히스토리(rusqlite 필드 XChaCha20 + keyed dedup).
//! - [`files`] — settings/workspace.json atomic write + MAC(D22).

pub mod files;
pub mod history;
pub mod keys;
pub mod keystore;

pub use history::{HistoryMeta, HistoryStore};
pub use keys::KeyManager;
pub use keystore::{FileKeyStore, KeyStore, MemoryKeyStore};

/// 저장 계층 오류.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("직렬화: {0}")]
    Serde(String),
    #[error("암호: {0}")]
    Crypto(String),
    #[error("저장소 손상: {0}")]
    Corrupt(String),
    #[error("MAC 검증 실패(변조 의심)")]
    MacMismatch,
}
