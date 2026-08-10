//! 설정 스키마 (§7.3, v2.0). serde_json 직렬화. 기본값은 "보안 최우선" 원칙.
//!
//! v1.1 대비 변경: 클라이언트 mDNS/`listen_port`/`manual_peers` 삭제(D6·D19),
//! `server`(주소·지문·워크스페이스 이름) 추가.

use serde::{Deserialize, Serialize};

use sb_proto::params::{DEFAULT_SERVER_PORT, MAX_CONTENT_SIZE_DEFAULT};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub sync: SyncSettings,
    #[serde(default)]
    pub history: HistorySettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub app: AppSettings,
}

fn one() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            sync: SyncSettings::default(),
            history: HistorySettings::default(),
            privacy: PrivacySettings::default(),
            server: ServerSettings::default(),
            app: AppSettings::default(),
        }
    }
}

impl Settings {
    /// 엔진이 쓰는 축약 설정.
    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            enabled: self.sync.enabled,
            sync_text: self.sync.sync_text,
            sync_images: self.sync.sync_images,
            sync_files: self.sync.sync_files,
            max_content_bytes: self.sync.max_content_bytes,
            history_cap: self.history.memory_max_items,
        }
    }

    pub fn to_json_pretty(&self) -> Result<String, crate::CoreError> {
        serde_json::to_string_pretty(self).map_err(|_| crate::CoreError::Settings)
    }

    /// 알 수 없는 키는 무시하되 존재하는 필드는 보존(round-trip, §7.2).
    pub fn from_json(s: &str) -> Result<Self, crate::CoreError> {
        serde_json::from_str(s).map_err(|_| crate::CoreError::Settings)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    #[serde(default = "tru")]
    pub enabled: bool,
    #[serde(default = "tru")]
    pub sync_text: bool,
    #[serde(default = "tru")]
    pub sync_images: bool,
    /// 파일 클립보드(Finder/탐색기 복사) 동기화. proto 3 이상 필요.
    #[serde(default = "tru")]
    pub sync_files: bool,
    #[serde(default = "default_max_content")]
    pub max_content_bytes: u64,
    #[serde(default = "tru")]
    pub auto_apply_received: bool,
    #[serde(default = "tru")]
    pub confirm_risky_content: bool,
}

fn tru() -> bool {
    true
}
fn default_max_content() -> u64 {
    MAX_CONTENT_SIZE_DEFAULT
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_content_bytes: MAX_CONTENT_SIZE_DEFAULT,
            auto_apply_received: true,
            confirm_risky_content: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySettings {
    #[serde(default = "thirty")]
    pub memory_max_items: usize,
    /// 디스크 영속화 기본 off (§7.2 D13).
    #[serde(default)]
    pub persist_enabled: bool,
    #[serde(default = "two_hundred")]
    pub max_items: usize,
    #[serde(default = "seven")]
    pub retention_days: u32,
    #[serde(default = "tru")]
    pub store_image_originals: bool,
    #[serde(default = "five_mib")]
    pub max_image_item_bytes: u64,
    #[serde(default = "twenty")]
    pub max_image_originals: usize,
}

fn thirty() -> usize {
    30
}
fn two_hundred() -> usize {
    200
}
fn seven() -> u32 {
    7
}
fn twenty() -> usize {
    20
}
fn five_mib() -> u64 {
    5 * 1024 * 1024
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            memory_max_items: 30,
            persist_enabled: false,
            max_items: 200,
            retention_days: 7,
            store_image_originals: true,
            max_image_item_bytes: 5 * 1024 * 1024,
            max_image_originals: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacySettings {
    #[serde(default = "tru")]
    pub exclude_concealed: bool,
    /// 힌트 미설정 매니저 대비 기본 동봉 목록(§4.6, D17).
    #[serde(default = "default_excluded_apps")]
    pub excluded_apps: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    /// 받은 파일에 "다른 기기에서 왔음" 표시(macOS quarantine / Windows Zone.Identifier).
    /// 실행형 파일을 무심코 여는 사고를 막는다. 사내에서만 쓰면 끌 수도 있다.
    #[serde(default = "tru")]
    pub mark_received_files: bool,
}

fn default_excluded_apps() -> Vec<String> {
    vec![
        "1Password".into(),
        "Bitwarden".into(),
        "KeePassXC".into(),
        "Dashlane".into(),
        "LastPass".into(),
        "Enpass".into(),
    ]
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            exclude_concealed: true,
            excluded_apps: default_excluded_apps(),
            exclude_patterns: Vec::new(),
            mark_received_files: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerSettings {
    /// 서버 주소 "host:port". 미설정 시 온보딩 필요.
    #[serde(default)]
    pub addr: Option<String>,
    /// 서버 TLS cert 지문(SHA-256 SPKI) hex — 초대 blob 경유로 확정.
    #[serde(default)]
    pub fingerprint_hex: Option<String>,
    /// 워크스페이스 이름(표시용).
    #[serde(default)]
    pub workspace_name: Option<String>,
    /// 이 기기가 릴레이 서버를 호스팅하는가(재시작 시 자동 재호스팅).
    #[serde(default)]
    pub host: bool,
    /// 호스팅 포트(기본 45871).
    #[serde(default)]
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "tru")]
    pub start_minimized_to_tray: bool,
    #[serde(default = "tru")]
    pub notify_on_receive: bool,
    #[serde(default = "tru")]
    pub notify_on_join: bool,
    #[serde(default = "info")]
    pub log_level: String,
    #[serde(default = "system")]
    pub language: String,
    #[serde(default = "system")]
    pub theme: String,
    /// null 이면 비식별 기본 장치명 사용(§7.3).
    #[serde(default)]
    pub device_name_override: Option<String>,
    /// 히스토리 팝업 글로벌 핫키(Tauri accelerator). 빈 문자열이면 등록하지 않는다.
    #[serde(default = "default_quick_hotkey")]
    pub quick_hotkey: String,
}

fn info() -> String {
    "info".into()
}
/// macOS 는 Cmd, Windows/Linux 는 Ctrl 로 해석된다.
pub fn default_quick_hotkey() -> String {
    "CmdOrCtrl+Shift+V".into()
}
fn system() -> String {
    "system".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            autostart: false,
            start_minimized_to_tray: true,
            notify_on_receive: true,
            notify_on_join: true,
            log_level: "info".into(),
            language: "system".into(),
            theme: "system".into(),
            device_name_override: None,
            quick_hotkey: default_quick_hotkey(),
        }
    }
}

/// 엔진 축약 설정.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub enabled: bool,
    pub sync_text: bool,
    pub sync_images: bool,
    pub sync_files: bool,
    pub max_content_bytes: u64,
    pub history_cap: usize,
}

/// 비식별 기본 장치명 — "<platform>-<hex4>" (§7.3, 호스트명 노출 방지).
pub fn default_device_name(platform: &str, id_suffix: &str) -> String {
    format!("{platform}-{}", &id_suffix[..id_suffix.len().min(4)])
}

/// 기본 서버 포트 상수 재노출(온보딩 UI 편의).
pub const DEFAULT_PORT: u16 = DEFAULT_SERVER_PORT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_security_first() {
        let s = Settings::default();
        assert!(s.sync.enabled);
        assert!(!s.history.persist_enabled, "히스토리 디스크 영속 기본 off");
        assert!(s.privacy.exclude_concealed);
        assert!(!s.privacy.excluded_apps.is_empty());
        assert!(s.sync.confirm_risky_content);
        assert!(!s.app.autostart, "자동 시작은 사용자가 켜야 함");
        assert_eq!(s.app.quick_hotkey, "CmdOrCtrl+Shift+V");
    }

    /// 기존 settings.json(핫키 필드 없음)을 읽어도 기본 핫키가 채워져야 한다.
    #[test]
    fn old_settings_file_gets_default_hotkey() {
        let old = r#"{ "app": { "autostart": false, "log_level": "info" } }"#;
        let s = Settings::from_json(old).unwrap();
        assert_eq!(s.app.quick_hotkey, "CmdOrCtrl+Shift+V");
    }

    #[test]
    fn json_roundtrip_and_partial() {
        let s = Settings::default();
        let js = s.to_json_pretty().unwrap();
        let back = Settings::from_json(&js).unwrap();
        assert_eq!(back.history.memory_max_items, 30);

        // 부분 JSON — 없는 필드는 기본값으로 채워짐.
        let partial = r#"{ "sync": { "enabled": false } }"#;
        let p = Settings::from_json(partial).unwrap();
        assert!(!p.sync.enabled);
        assert!(p.sync.sync_text, "누락 필드는 기본 true");
        assert_eq!(p.history.memory_max_items, 30);
    }

    #[test]
    fn device_name_is_nonidentifying() {
        assert_eq!(default_device_name("mac", "3f7abc"), "mac-3f7a");
    }
}
