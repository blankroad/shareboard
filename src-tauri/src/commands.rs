//! Tauri 커맨드 (UI → Rust). 대부분 Core 뮤텍스만 잠근다.

use tauri::{AppHandle, Emitter, State};

use sb_clipboard::{ClipContent, ClipboardAccess};
use sb_core::history::Origin;
use sb_proto::{ContentKind, C2s};

use crate::core::*;

/// 상태 스냅샷 생성.
pub fn status_of(core: &Core) -> StatusView {
    let online = core.members.iter().filter(|m| m.online).count();
    StatusView {
        connected: core.connected,
        server_addr: core.server_addr(),
        workspace_name: core.settings.server.workspace_name.clone(),
        member_count: core.members.len(),
        online_count: online,
        sync_enabled: core.settings.sync.enabled,
        gk_present: core.gk_present,
        device_id: hex(&core.device_id()),
    }
}

/// 상태 변경 이벤트 발행(무시 가능한 에러).
pub fn emit_status(app: &AppHandle, core: &Core) {
    let _ = app.emit("status-changed", status_of(core));
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, String> {
    let core = state.lock().await;
    Ok(AppInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        proto_min: sb_proto::params::PROTO_MIN,
        proto_max: sb_proto::params::PROTO_MAX,
        device_id: hex(&core.device_id()),
    })
}

#[tauri::command]
pub async fn get_status(state: State<'_, AppState>) -> Result<StatusView, String> {
    Ok(status_of(&*state.lock().await))
}

#[tauri::command]
pub async fn get_members(state: State<'_, AppState>) -> Result<Vec<MemberView>, String> {
    Ok(state.lock().await.members.clone())
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<sb_core::Settings, String> {
    Ok(state.lock().await.settings.clone())
}

#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: sb_core::Settings,
) -> Result<(), String> {
    let mut core = state.lock().await;
    core.settings = settings;
    let enabled = core.settings.sync.enabled;
    core.engine.set_enabled(enabled);
    // 디스크 저장.
    let path = core.data_dir.join("settings.json");
    sb_store::files::save_json(&path, &core.settings).map_err(|e| e.to_string())?;
    let need_reconnect = core.settings.server.addr.is_some();
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    if need_reconnect {
        rc.notify_one();
    }
    Ok(())
}

#[tauri::command]
pub async fn set_sync_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let mut core = state.lock().await;
    core.settings.sync.enabled = enabled;
    core.engine.set_enabled(enabled);
    let path = core.data_dir.join("settings.json");
    let _ = sb_store::files::save_json(&path, &core.settings);
    emit_status(&app, &core);
    Ok(())
}

#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> Result<Vec<HistoryItemView>, String> {
    let core = state.lock().await;
    let out = core
        .engine
        .history()
        .list()
        .map(|it| HistoryItemView {
            id: hex(&it.id),
            kind: match it.kind {
                ContentKind::Text => "text".into(),
                ContentKind::ImagePng => "image".into(),
            },
            origin: match it.origin {
                Origin::Local => "local".into(),
                Origin::Peer(d) => sb_proto::short_id(&d),
            },
            size: it.size,
            created_at: it.created_at_ms,
            preview: it.preview.clone(),
            pinned: it.pinned,
            has_body: core.body_cache.contains_key(&it.id),
        })
        .collect();
    Ok(out)
}

#[tauri::command]
pub async fn copy_history_item(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let id = hex32(&id).ok_or("잘못된 id")?;
    let core = state.lock().await;
    let Some(bytes) = core.body_cache.get(&id).cloned() else {
        return Ok(false);
    };
    let kind = core
        .engine
        .history()
        .get(&id)
        .map(|i| i.kind)
        .unwrap_or(ContentKind::Text);
    drop(core);
    // OS 클립보드에 쓰기 → 워커의 폴이 감지해 자연스럽게 재전파.
    let content = ClipContent { kind, bytes };
    sb_clipboard::arboard_backend::ArboardAccess::new()
        .write(&content)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn delete_history_item(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let id = hex32(&id).ok_or("잘못된 id")?;
    let mut core = state.lock().await;
    core.engine.history_mut().delete(&id);
    core.body_cache.remove(&id);
    let _ = core.history.delete(&id);
    let _ = app.emit("history-updated", ());
    Ok(())
}

#[tauri::command]
pub async fn set_pinned(state: State<'_, AppState>, id: String, pinned: bool) -> Result<(), String> {
    let id = hex32(&id).ok_or("잘못된 id")?;
    let mut core = state.lock().await;
    core.engine.history_mut().set_pinned(&id, pinned);
    let _ = core.history.set_pinned(&id, pinned);
    Ok(())
}

#[tauri::command]
pub async fn clear_history(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut core = state.lock().await;
    core.engine.history_mut().clear();
    core.body_cache.clear();
    let _ = core.history.clear();
    let _ = app.emit("history-updated", ());
    Ok(())
}

/// 서버 주소·지문 설정(온보딩 공통) → 재연결.
#[tauri::command]
pub async fn configure_server(
    app: AppHandle,
    state: State<'_, AppState>,
    addr: String,
    fingerprint_hex: String,
    workspace_name: Option<String>,
) -> Result<(), String> {
    let fp = hex32(&fingerprint_hex).ok_or("잘못된 지문(64 hex 필요)")?;
    let mut core = state.lock().await;
    core.settings.server.addr = Some(addr);
    core.settings.server.fingerprint_hex = Some(fingerprint_hex);
    if workspace_name.is_some() {
        core.settings.server.workspace_name = workspace_name;
    }
    core.server_fp = Some(fp);
    let path = core.data_dir.join("settings.json");
    sb_store::files::save_json(&path, &core.settings).map_err(|e| e.to_string())?;
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 워크스페이스 생성(창립자). GK 생성·genesis 준비 → 재연결 시 클레임.
#[tauri::command]
pub async fn create_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    setup_token: String,
    now_ms: u64,
) -> Result<(), String> {
    let mut core = state.lock().await;
    let (genesis, wid) = sb_crypto::wslog::build_genesis(&core.identity, &name, now_ms);
    let genesis_bytes = sb_crypto::wslog::entry_bytes(&genesis);
    // GK_0 생성 → 엔진·keystore.
    let gk = sb_crypto::GroupKey::generate(0);
    persist_group_key(&core, &gk).map_err(|e| e.to_string())?;
    core.engine.set_group_key(gk);
    core.gk_present = true;
    core.workspace_id = Some(wid);
    core.settings.server.workspace_name = Some(name);
    core.log = vec![genesis_bytes.clone()];
    core.pending = Some(PendingAction::Claim { genesis: genesis_bytes, token: setup_token });
    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 조인(새 기기). 코드 저장 → 재연결 시 게스트 조인 플로우.
#[tauri::command]
pub async fn join_workspace(
    state: State<'_, AppState>,
    code: String,
) -> Result<(), String> {
    let mut core = state.lock().await;
    core.pending = Some(PendingAction::Join { code });
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 초대 발급 → PutInvite. 반환 = 표시용 코드.
#[tauri::command]
pub async fn generate_invite(state: State<'_, AppState>, expires_at: u64) -> Result<String, String> {
    let core = state.lock().await;
    if !core.connected {
        return Err("서버에 연결되어야 합니다".into());
    }
    let wid = core.workspace_id.ok_or("워크스페이스 없음")?;
    let fp = core.server_fp.ok_or("서버 지문 없음")?;
    let head = core.log_head().1;
    let (code, locator, blob) =
        sb_crypto::make_invite(&core.identity, wid, head, fp, expires_at).map_err(|e| e.to_string())?;
    let display = sb_crypto::invite::format_display(&code);
    if let Some(out) = &core.out {
        out.send(C2s::PutInvite { locator, blob, ttl_s: sb_proto::params::INVITE_TTL_DEFAULT_S })
            .await
            .map_err(|_| "전송 실패")?;
    } else {
        return Err("연결 없음".into());
    }
    Ok(display)
}

/// keystore 에 현재 GK 저장(에폭 + 32B).
pub fn persist_group_key(core: &Core, gk: &sb_crypto::GroupKey) -> Result<(), String> {
    let store = crate::worker::keystore_for(&core.data_dir);
    use sb_store::KeyStore;
    let mut buf = gk.epoch().to_le_bytes().to_vec();
    buf.extend_from_slice(gk.expose());
    store.set("group.key", &buf).map_err(|e| e.to_string())
}
