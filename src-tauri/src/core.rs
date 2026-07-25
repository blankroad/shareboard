//! 앱 상태 — 모든 코어 크레이트를 하나로 묶는다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc, Mutex, Notify};

use sb_core::{Settings, SyncEngine};
use sb_crypto::Identity;
use sb_proto::{C2s, ContentId, DeviceId};
use sb_store::HistoryStore;

pub type AppState = Arc<Mutex<Core>>;

/// 온보딩 등 연결 시 워커가 수행할 대기 작업.
#[derive(Clone)]
pub enum PendingAction {
    /// 워크스페이스 생성(창립자) — genesis 바이트 + setup 토큰.
    Claim { genesis: Vec<u8>, token: String },
    /// 조인(새 기기) — 초대 코드.
    Join { code: String },
}

pub struct Core {
    pub identity: Arc<Identity>,
    pub settings: Settings,
    pub engine: SyncEngine,
    pub history: HistoryStore,
    /// 재복사를 위한 본문 캐시(메모리). 디스크 영속은 settings.history.persist_enabled 시 sb-store.
    pub body_cache: HashMap<ContentId, Vec<u8>>,
    pub members: Vec<MemberView>,
    pub server_fp: Option<[u8; 32]>,
    pub connected: bool,
    pub gk_present: bool,
    /// 현재 그룹 키(엔진과 동일 사본, 조인자 wrap 발급용).
    pub current_gk: Option<sb_crypto::GroupKey>,
    pub pending: Option<PendingAction>,
    /// 현재 워크스페이스 로그(원시 엔트리 바이트) + 식별자.
    pub log: Vec<Vec<u8>>,
    pub workspace_id: Option<[u8; 32]>,
    /// 워커의 서버 송신 채널(연결 시 세팅).
    pub out: Option<mpsc::Sender<C2s>>,
    pub data_dir: PathBuf,
    /// 설정 변경 시 워커를 깨워 재연결시키는 신호.
    pub reconnect: Arc<Notify>,
}

impl Core {
    pub fn device_id(&self) -> DeviceId {
        self.identity.device_id()
    }

    /// 본문 캐시에 추가(상한 유지).
    pub fn cache_body(&mut self, id: ContentId, bytes: Vec<u8>) {
        let cap = self.settings.history.memory_max_items.max(1);
        if self.body_cache.len() >= cap * 2 {
            // 단순 정리: 절반 비우기(정확한 LRU 대신).
            self.body_cache.clear();
        }
        self.body_cache.insert(id, bytes);
    }

    pub fn server_addr(&self) -> Option<String> {
        self.settings.server.addr.clone()
    }

    /// 로그 head (seq, hash). 비어있으면 (0, zero).
    pub fn log_head(&self) -> (u64, [u8; 32]) {
        match self.log.last() {
            Some(bytes) => ((self.log.len() as u64) - 1, sb_crypto::wslog::entry_hash(bytes)),
            None => (0, [0u8; 32]),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct MemberView {
    pub device_id: String,
    pub name: String,
    pub online: bool,
    pub platform: String,
}

/// 프런트로 보내는 상태 스냅샷.
#[derive(Clone, Serialize)]
pub struct StatusView {
    pub connected: bool,
    pub server_addr: Option<String>,
    pub workspace_name: Option<String>,
    pub member_count: usize,
    pub online_count: usize,
    pub sync_enabled: bool,
    pub gk_present: bool,
    pub device_id: String,
}

#[derive(Clone, Serialize)]
pub struct HistoryItemView {
    pub id: String,
    pub kind: String,
    pub origin: String,
    pub size: u64,
    pub created_at: u64,
    pub preview: String,
    pub pinned: bool,
    pub has_body: bool,
}

#[derive(Clone, Serialize)]
pub struct AppInfo {
    pub app_version: String,
    pub proto_min: u16,
    pub proto_max: u16,
    pub device_id: String,
}

/// hex 인코딩.
pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// hex → 32바이트.
pub fn hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, c) in s.as_bytes().chunks(2).enumerate() {
        let hi = (c[0] as char).to_digit(16)?;
        let lo = (c[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}
