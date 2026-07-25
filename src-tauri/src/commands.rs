//! Tauri 커맨드 (UI → Rust). 대부분 Core 뮤텍스만 잠근다.

use tauri::{AppHandle, Emitter, State};

use sb_clipboard::{ClipContent, ClipboardAccess};
use sb_core::history::Origin;
use sb_proto::{C2s, ContentKind};

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
        hosting: core.hosting,
        host_addr: core.host_addr.clone(),
        host_fingerprint: core.host_fp.map(|f| hex(&f)),
        joining: core.joining,
        join_error: core.join_error.clone(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 이 기기를 서버로 삼아 워크스페이스를 새로 만든다(내장 릴레이 시작 + 창립자).
#[tauri::command]
pub async fn host_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<HostInfo, String> {
    let token = sb_crypto::invite::generate_code();
    let hash = sb_crypto::hash::sha256(token.as_bytes());
    // 내장 서버 시작.
    let (addr, fp) = crate::worker::start_embedded_server(state.inner(), Some(hash))
        .await
        .map_err(|e| e.to_string())?;

    // 창립자로서 워크스페이스 생성 → 워커가 재연결하며 Claim.
    let mut core = state.lock().await;
    let (genesis, wid) = sb_crypto::wslog::build_genesis(&core.identity, &name, now_ms());
    let gbytes = sb_crypto::wslog::entry_bytes(&genesis);
    let gk = sb_crypto::GroupKey::generate(0);
    persist_group_key(&core, &gk).map_err(|e| e.to_string())?;
    core.engine.set_group_key(gk.clone());
    core.current_gk = Some(gk);
    core.gk_present = true;
    core.workspace_id = Some(wid);
    core.settings.server.workspace_name = Some(name);
    core.log = vec![gbytes.clone()];
    core.pending = Some(PendingAction::Claim {
        genesis: gbytes,
        token: (*token).to_string(),
    });
    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(HostInfo {
        addr,
        fingerprint_hex: hex(&fp),
    })
}

/// 현재 호스팅 정보(공유용). 호스팅 아니면 None.
#[tauri::command]
pub async fn get_host_info(state: State<'_, AppState>) -> Result<Option<HostInfo>, String> {
    let c = state.lock().await;
    if c.hosting {
        Ok(Some(HostInfo {
            addr: c.host_addr.clone().unwrap_or_default(),
            fingerprint_hex: c.host_fp.map(|f| hex(&f)).unwrap_or_default(),
        }))
    } else {
        Ok(None)
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
    core.pending = Some(PendingAction::Claim {
        genesis: genesis_bytes,
        token: setup_token,
    });
    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 조인(새 기기). 코드 저장 → 재연결 시 게스트 조인 플로우.
#[tauri::command]
pub async fn join_workspace(app: AppHandle, state: State<'_, AppState>, code: String) -> Result<(), String> {
    let mut core = state.lock().await;
    core.pending = Some(PendingAction::Join { code });
    core.joining = true;
    core.join_error = None;
    emit_status(&app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 온보딩 초기화 — 조인 실패/취소 후 서버 설정을 지우고 온보딩으로 복귀.
#[tauri::command]
pub async fn reset_onboarding(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut core = state.lock().await;
    core.settings.server.addr = None;
    core.settings.server.fingerprint_hex = None;
    core.settings.server.workspace_name = None;
    core.server_fp = None;
    core.pending = None;
    core.joining = false;
    core.join_error = None;
    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
    emit_status(&app, &core);
    Ok(())
}

/// 초대 발급 재료(락을 잡은 채 await 하지 않도록 소유값만 추출 — Core 는 !Sync).
struct InviteCtx {
    identity: std::sync::Arc<sb_crypto::Identity>,
    wid: [u8; 32],
    server_fp: [u8; 32],
    head: [u8; 32],
    out: tokio::sync::mpsc::Sender<C2s>,
}

fn invite_ctx(core: &Core) -> Result<InviteCtx, String> {
    if !core.connected {
        return Err("서버에 연결되어야 합니다".into());
    }
    Ok(InviteCtx {
        identity: core.identity.clone(),
        wid: core.workspace_id.ok_or("워크스페이스 없음")?,
        server_fp: core.server_fp.ok_or("서버 지문 없음")?,
        head: core.log_head().1,
        out: core.out.clone().ok_or("연결 없음")?,
    })
}

/// 초대 발급(공용): make_invite + PutInvite. 반환 = 정규형 12자 코드. 락 해제 후 호출.
async fn issue_invite(ctx: InviteCtx) -> Result<String, String> {
    let expires_at = now_ms() + (sb_proto::params::INVITE_TTL_DEFAULT_S as u64) * 1000;
    let (code, locator, blob) =
        sb_crypto::make_invite(&ctx.identity, ctx.wid, ctx.head, ctx.server_fp, expires_at)
            .map_err(|e| e.to_string())?;
    let canonical = (*code).to_string();
    ctx.out
        .send(C2s::PutInvite {
            locator,
            blob,
            ttl_s: sb_proto::params::INVITE_TTL_DEFAULT_S,
        })
        .await
        .map_err(|_| "전송 실패".to_string())?;
    Ok(canonical)
}

/// 초대 발급 → 표시용 코드(수동 입력 폴백용).
#[tauri::command]
pub async fn generate_invite(state: State<'_, AppState>, expires_at: u64) -> Result<String, String> {
    let _ = expires_at; // TTL 은 기본값 사용
    let ctx = {
        let core = state.lock().await;
        invite_ctx(&core)?
    };
    let code = issue_invite(ctx).await?;
    Ok(sb_crypto::invite::format_display(&code))
}

/// 초대 링크 생성 — 서버 주소·지문·코드를 한 문자열로 묶는다(교환 편의, §8.2).
#[tauri::command]
pub async fn generate_invite_link(state: State<'_, AppState>) -> Result<String, String> {
    let (addr, fp_hex, ctx) = {
        let core = state.lock().await;
        let addr = core.settings.server.addr.clone().ok_or("서버 미설정")?;
        let fp_hex = core
            .settings
            .server
            .fingerprint_hex
            .clone()
            .ok_or("서버 지문 없음")?;
        (addr, fp_hex, invite_ctx(&core)?)
    };
    let code = issue_invite(ctx).await?;
    Ok(format!("shareboard://join?a={addr}&f={fp_hex}&c={code}"))
}

/// 초대 링크 파싱 → (addr, fingerprint_hex, code).
fn parse_invite_link(link: &str) -> Option<(String, String, String)> {
    let q = link.trim().strip_prefix("shareboard://join?")?;
    let (mut a, mut f, mut c) = (None, None, None);
    for kv in q.split('&') {
        let (k, v) = kv.split_once('=')?;
        match k {
            "a" => a = Some(v.to_string()),
            "f" => f = Some(v.to_string()),
            "c" => c = Some(v.to_string()),
            _ => {}
        }
    }
    Some((a?, f?, c?))
}

/// 초대 링크 적용(공용) — UI 커맨드와 딥링크 핸들러가 공유.
pub async fn apply_join_link(app: &AppHandle, state: &AppState, link: &str) -> Result<(), String> {
    let (addr, fp_hex, code) =
        parse_invite_link(link).ok_or("초대 링크 형식이 올바르지 않습니다 (shareboard://…)")?;
    let fp = hex32(&fp_hex).ok_or("링크의 서버 지문이 올바르지 않습니다")?;
    let mut core = state.lock().await;
    core.settings.server.addr = Some(addr);
    core.settings.server.fingerprint_hex = Some(fp_hex);
    core.server_fp = Some(fp);
    core.pending = Some(PendingAction::Join { code });
    core.joining = true;
    core.join_error = None;
    let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
    emit_status(app, &core);
    let rc = core.reconnect.clone();
    drop(core);
    rc.notify_one();
    Ok(())
}

/// 초대 링크 하나로 참여(주소·지문·코드 자동 설정 → 조인).
#[tauri::command]
pub async fn join_by_link(app: AppHandle, state: State<'_, AppState>, link: String) -> Result<(), String> {
    apply_join_link(&app, state.inner(), &link).await
}

/// keystore 에 현재 GK 저장(에폭 + 32B).
pub fn persist_group_key(core: &Core, gk: &sb_crypto::GroupKey) -> Result<(), String> {
    let store = crate::worker::keystore_for(&core.data_dir);
    use sb_store::KeyStore;
    let mut buf = gk.epoch().to_le_bytes().to_vec();
    buf.extend_from_slice(gk.expose());
    store.set("group.key", &buf).map_err(|e| e.to_string())
}
