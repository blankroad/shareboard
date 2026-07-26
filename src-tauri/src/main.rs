// shareboard 데스크톱 앱 — 트레이 상주, 백그라운드 워커가 서버 연결 + 클립보드 동기화.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod commands;
mod core;
mod worker;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tokio::sync::{Mutex, Notify};

use sb_core::{Settings, SyncEngine};
use sb_crypto::GroupKey;
use sb_store::{HistoryStore, KeyManager, KeyStore};

use crate::core::{AppState, Core};

fn data_dir() -> std::path::PathBuf {
    // 여러 인스턴스 테스트: SHAREBOARD_DATA_DIR 로 데이터 디렉터리 분리 가능.
    if let Ok(d) = std::env::var("SHAREBOARD_DATA_DIR") {
        if !d.is_empty() {
            return std::path::PathBuf::from(d);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("shareboard")
}

fn build_core() -> Core {
    let dd = data_dir();
    std::fs::create_dir_all(&dd).ok();

    let ks = worker::keystore_for(&dd);
    let km = KeyManager::new(&ks);
    let identity = km.load_or_create_identity().expect("identity 생성/로드");
    let history_key = *km.history_key().expect("history key");
    let dedup_key = *km.dedup_key().expect("dedup key");
    let _ = km.ws_mac_key();

    let settings: Settings = sb_store::files::load_json(&dd.join("settings.json"))
        .ok()
        .flatten()
        .unwrap_or_default();

    let (gk, gk_present) = match ks.get("group.key").ok().flatten() {
        Some(b) if b.len() == 40 => {
            let epoch = u64::from_le_bytes(b[..8].try_into().unwrap());
            let mut key = [0u8; 32];
            key.copy_from_slice(&b[8..40]);
            (GroupKey::from_bytes(epoch, key), true)
        }
        _ => (GroupKey::from_bytes(0, [0u8; 32]), false),
    };

    let engine = SyncEngine::new(identity.device_id(), gk.clone(), settings.engine_config());
    let history =
        HistoryStore::open_path(&dd.join("history.db"), history_key, dedup_key).expect("history db");
    let server_fp = settings
        .server
        .fingerprint_hex
        .as_deref()
        .and_then(crate::core::hex32);

    Core {
        identity: Arc::new(identity),
        settings,
        engine,
        history,
        body_cache: Default::default(),
        members: Vec::new(),
        server_fp,
        connected: false,
        gk_present,
        hosting: false,
        host_addr: None,
        host_fp: None,
        joining: false,
        join_error: None,
        current_gk: if gk_present { Some(gk) } else { None },
        pending: None,
        log: Vec::new(),
        workspace_id: None,
        out: None,
        data_dir: dd,
        reconnect: Arc::new(Notify::new()),
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "창 열기", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle_sync", "동기화 켬/끔", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &quit])?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray/tray-template-44.png"))
        .expect("tray icon");

    let _tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("shareboard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
            "toggle_sync" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let mut core = state.lock().await;
                    let v = !core.settings.sync.enabled;
                    core.settings.sync.enabled = v;
                    core.engine.set_enabled(v);
                    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
                    crate::commands::emit_status(&app, &core);
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    sb_net::init_crypto();

    let state: AppState = Arc::new(Mutex::new(build_core()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(state.clone())
        .setup(move |app| {
            setup_tray(app)?;

            // shareboard:// 딥링크 자동 참여.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_handle = app.handle().clone();
                let st = state.clone();
                // 개발 빌드에서 현재 실행 파일을 스킴 핸들러로 등록 시도(플랫폼별 best-effort).
                let _ = app.deep_link().register("shareboard");
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let link = url.to_string();
                        if link.starts_with("shareboard://") {
                            let app_handle = app_handle.clone();
                            let st = st.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(e) = commands::apply_join_link(&app_handle, &st, &link).await {
                                    tracing::warn!("딥링크 참여 실패: {e}");
                                }
                                if let Some(w) = app_handle.get_webview_window("main") {
                                    let _ = w.show();
                                    let _ = w.set_focus();
                                }
                            });
                        }
                    }
                });
            }

            let handle = app.handle().clone();
            let st = state.clone();
            tauri::async_runtime::spawn(async move {
                worker::run(handle, st).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // 창 닫기 = 숨김(트레이 상주). WebView 메모리는 유지되나 v1 수용.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_status,
            commands::get_members,
            commands::revoke_member,
            commands::get_settings,
            commands::update_settings,
            commands::set_sync_enabled,
            commands::get_history,
            commands::copy_history_item,
            commands::delete_history_item,
            commands::set_pinned,
            commands::clear_history,
            commands::configure_server,
            commands::create_workspace,
            commands::join_workspace,
            commands::generate_invite,
            commands::generate_invite_link,
            commands::join_by_link,
            commands::reset_onboarding,
            commands::host_workspace,
            commands::get_host_info,
        ])
        .run(tauri::generate_context!())
        .expect("shareboard 실행 실패");
}
