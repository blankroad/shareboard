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
    /// 재복사를 위한 본문 캐시(메모리 전용 — 히스토리는 디스크에 남기지 않는다).
    /// **바이트 상한**을 지킨다 — 파일 클립이 들어오면서 개수 상한만으로는 메모리가 GB 단위로
    /// 부풀 수 있다(30개 × 32MB). `cache_body`/`forget_body`/`clear_bodies` 로만 만진다.
    pub body_cache: HashMap<ContentId, Vec<u8>>,
    /// 축출 순서(FIFO) + 현재 캐시 바이트 합. 초기화 외에는 위 메서드로만 만진다.
    pub(crate) body_order: std::collections::VecDeque<ContentId>,
    pub(crate) body_bytes: u64,
    /// 이미지 항목 썸네일(data URL) 캐시. 본문에서 파생되므로 삭제 시 함께 정리한다.
    pub thumb_cache: HashMap<ContentId, ThumbView>,
    pub members: Vec<MemberView>,
    pub server_fp: Option<[u8; 32]>,
    pub connected: bool,
    pub gk_present: bool,
    /// 이 기기가 릴레이 서버를 호스팅 중인가.
    pub hosting: bool,
    /// 호스팅 시 다른 사람이 접속할 주소 / 서버 지문(공유용).
    pub host_addr: Option<String>,
    pub host_fp: Option<[u8; 32]>,
    /// 조인 진행 중 / 마지막 조인 실패 사유(UI 확인용).
    pub joining: bool,
    pub join_error: Option<String>,
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
        // 바이트 상한 = 4 × 최대 콘텐츠 크기(§5.6 CONTENT_CACHE_BYTES 취지) — 최근 몇 건은
        // 재복사 가능하게 두되, 큰 파일이 쌓여 메모리를 삼키지는 않게 한다.
        let cap_bytes = self.settings.sync.max_content_bytes.saturating_mul(4).max(1);
        let cap_items = self.settings.history.memory_max_items.max(1) * 2;

        self.forget_body(&id); // 같은 id 재삽입 시 이중 계산 방지
        self.body_bytes = self.body_bytes.saturating_add(bytes.len() as u64);
        self.body_cache.insert(id, bytes);
        self.body_order.push_back(id);

        while self.body_bytes > cap_bytes || self.body_order.len() > cap_items {
            let Some(old) = self.body_order.pop_front() else {
                break;
            };
            if let Some(dropped) = self.body_cache.remove(&old) {
                self.body_bytes = self.body_bytes.saturating_sub(dropped.len() as u64);
                self.thumb_cache.remove(&old);
            }
            // 방금 넣은 것만 남았는데도 상한을 넘으면(한 건이 상한보다 큼) 더 버릴 게 없다.
            if self.body_order.is_empty() {
                break;
            }
        }
    }

    /// 본문 하나를 캐시에서 지운다(바이트 합 동기화 포함).
    pub fn forget_body(&mut self, id: &ContentId) {
        if let Some(b) = self.body_cache.remove(id) {
            self.body_bytes = self.body_bytes.saturating_sub(b.len() as u64);
            self.body_order.retain(|x| x != id);
        }
        self.thumb_cache.remove(id);
    }

    /// 본문·썸네일 캐시 전체 비우기.
    pub fn clear_bodies(&mut self) {
        self.body_cache.clear();
        self.body_order.clear();
        self.body_bytes = 0;
        self.thumb_cache.clear();
    }

    /// 현재 본문 캐시가 쓰는 바이트(진단용).
    pub fn body_cache_bytes(&self) -> u64 {
        self.body_bytes
    }

    /// 썸네일 캐시 상한 유지 — 히스토리에서 사라진 항목의 썸네일을 버린다.
    pub fn prune_thumbs(&mut self) {
        let cap = self.settings.history.memory_max_items.max(1) * 2;
        if self.thumb_cache.len() <= cap {
            return;
        }
        let alive: std::collections::HashSet<ContentId> =
            self.engine.history().list().map(|i| i.id).collect();
        self.thumb_cache.retain(|id, _| alive.contains(id));
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
    /// E2E 프로필에서 복호한 기기 이름(있으면).
    pub name: Option<String>,
    pub online: bool,
    pub platform: String,
    /// 서버가 스탬프한 접속 주소(사람 식별용).
    pub addr: Option<String>,
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
    /// 이 기기가 워크스페이스 창립자인지(강퇴 등 관리 액션 노출용).
    pub is_founder: bool,
    pub hosting: bool,
    pub host_addr: Option<String>,
    pub host_fingerprint: Option<String>,
    pub joining: bool,
    pub join_error: Option<String>,
}

/// 호스팅 정보(공유용).
#[derive(Clone, Serialize)]
pub struct HostInfo {
    pub addr: String,
    pub fingerprint_hex: String,
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

/// 히스토리 이미지 썸네일. `width`/`height` 는 **원본** 크기(UI 라벨용).
#[derive(Clone, Serialize)]
pub struct ThumbView {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
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
