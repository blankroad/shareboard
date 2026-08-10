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
        is_founder: sb_crypto::wslog::verify_chain(&core.log, 0)
            .map(|v| v.creator == core.device_id())
            .unwrap_or(false),
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

    let mut core = state.lock().await;
    // 이미 이 기기가 만든 워크스페이스가 있으면(역할 재설정 후 재호스팅 등) 다시 클레임하지
    // 않는다 — 내장 서버는 로그를 영속하므로 이미 claimed 이고 재클레임은 거부된다.
    // 기존 로그·GK 를 그대로 두고 재연결만 시킨다.
    if !core.log.is_empty() {
        if core.settings.server.workspace_name.is_none() {
            core.settings.server.workspace_name = Some(name);
        }
        let _ = sb_store::files::save_json(&core.data_dir.join("settings.json"), &core.settings);
        emit_status(&app, &core);
        let rc = core.reconnect.clone();
        drop(core);
        rc.notify_one();
        return Ok(HostInfo {
            addr,
            fingerprint_hex: hex(&fp),
        });
    }

    // 창립자로서 워크스페이스 생성 → 워커가 재연결하며 Claim.
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
    // 기기 이름이 바뀌었을 수 있으니 프로필 재발행.
    crate::worker::send_profile(state.inner()).await;
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

/// 이미지 항목 썸네일(data URL). 이미지가 아니거나 본문이 캐시에 없으면 `None`.
/// UI 는 항목별로 지연 호출하고, 생성 결과는 앱이 캐시한다.
#[tauri::command]
pub async fn get_thumbnail(state: State<'_, AppState>, id: String) -> Result<Option<ThumbView>, String> {
    let id = hex32(&id).ok_or("잘못된 id")?;
    let core = state.lock().await;
    if let Some(t) = core.thumb_cache.get(&id) {
        return Ok(Some(t.clone()));
    }
    let is_image = core
        .engine
        .history()
        .get(&id)
        .is_some_and(|i| i.kind == ContentKind::ImagePng);
    if !is_image {
        return Ok(None);
    }
    let Some(bytes) = core.body_cache.get(&id).cloned() else {
        return Ok(None); // 원격 이미지 본문 도착 전 — UI 가 나중에 다시 요청한다.
    };
    drop(core); // 디코딩은 락 밖에서(대용량 PNG).

    let thumb = tokio::task::spawn_blocking(move || crate::thumb::render(&bytes))
        .await
        .map_err(|e| e.to_string())?;

    if let Some(t) = thumb.clone() {
        let mut core = state.lock().await;
        core.thumb_cache.insert(id, t);
        core.prune_thumbs();
    }
    Ok(thumb)
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
    core.thumb_cache.remove(&id);
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
    core.thumb_cache.clear();
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

/// 워크스페이스 생성(창립자) — 별도로 돌고 있는 `sb-server` 에 setup 토큰으로 클레임한다.
///
/// 주소·지문 설정과 genesis/GK 준비를 **한 커맨드로** 처리한다: 둘을 나누면 그 사이에 워커가
/// pending 없이 재연결해 게스트 레인에서 Hello 를 던지는 어중간한 상태가 생긴다.
/// 실제 클레임 성공 여부는 워커가 AppendAck 를 받아 확인하고, 실패하면 온보딩으로 되돌린다.
#[tauri::command]
pub async fn create_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    addr: String,
    fingerprint_hex: String,
    name: String,
    setup_token: String,
    now_ms: u64,
) -> Result<(), String> {
    let fp = hex32(&fingerprint_hex).ok_or("잘못된 지문(64 hex 필요)")?;
    if setup_token.trim().is_empty() {
        return Err("setup 토큰을 입력하세요".into());
    }
    let mut core = state.lock().await;
    if core.hosting {
        return Err("이 기기가 이미 서버를 호스팅 중입니다".into());
    }
    let (genesis, wid) = sb_crypto::wslog::build_genesis(&core.identity, &name, now_ms);
    let genesis_bytes = sb_crypto::wslog::entry_bytes(&genesis);
    // GK_0 생성 → 엔진·keystore·current_gk(조인자 wrap 발급·프로필 봉인에 필요).
    let gk = sb_crypto::GroupKey::generate(0);
    persist_group_key(&core, &gk).map_err(|e| e.to_string())?;
    core.engine.set_group_key(gk.clone());
    core.current_gk = Some(gk);
    core.gk_present = true;
    core.workspace_id = Some(wid);
    core.settings.server.addr = Some(addr);
    core.settings.server.fingerprint_hex = Some(fingerprint_hex);
    core.settings.server.workspace_name = Some(name);
    core.server_fp = Some(fp);
    core.log = vec![genesis_bytes.clone()];
    core.pending = Some(PendingAction::Claim {
        genesis: genesis_bytes,
        token: setup_token,
    });
    core.joining = true;
    core.join_error = None;
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

/// 강퇴 재료(락을 잡은 채 await 하지 않도록 소유값만 추출 — Core 는 !Sync).
struct RevokeCtx {
    identity: std::sync::Arc<sb_crypto::Identity>,
    wid: [u8; 32],
    out: tokio::sync::mpsc::Sender<C2s>,
    log: Vec<Vec<u8>>,
}

/// 멤버 강퇴(§4.4) — Remove + Epoch 회전 + 남은 멤버에게 새 GK 재-wrap 배달.
///
/// 강퇴 대상은 제거 후 roster 에서 빠져 GK_{e+1} 을 받지 못하므로 접근만 상실한다.
/// (로컬 키 자동 파기는 없음 — §5.4 는 검증된 Remove(self)+명시 확인 이중 게이트에서만.)
/// 서버는 blind relay 라 변경 불필요: 기존 AppendEntry + PutKeyUpdate 중계로 충분하며,
/// LogAppended 는 송신자를 제외한 현재 roster 로만 팬아웃되어 강퇴 대상은 통지받지 않는다.
#[tauri::command]
pub async fn revoke_member(
    app: AppHandle,
    state: State<'_, AppState>,
    device_id: String,
) -> Result<(), String> {
    let target = hex32(&device_id).ok_or("잘못된 device_id")?;

    // ── 락 구간: 검증 + 소유값 추출 ──
    let ctx = {
        let core = state.lock().await;
        if !core.connected {
            return Err("서버에 연결되어야 합니다".into());
        }
        if core.current_gk.is_none() {
            return Err("그룹 키 없음".into());
        }
        if target == core.device_id() {
            return Err("자기 자신은 강퇴할 수 없습니다".into());
        }
        RevokeCtx {
            identity: core.identity.clone(),
            wid: core.workspace_id.ok_or("워크스페이스 없음")?,
            out: core.out.clone().ok_or("연결 없음")?,
            log: core.log.clone(),
        }
    };

    // ── 락 밖: crypto 시퀀스(remove → epoch → rotation, 정본 = wslog::remove_then_epoch_rotates) ──
    use sb_crypto::wslog;
    let me = &ctx.identity;
    let v = wslog::verify_chain(&ctx.log, 0).map_err(|e| e.to_string())?;
    if !v.is_member(&target) {
        return Err("대상이 현재 멤버가 아닙니다".into());
    }

    // 1) Remove(잔존 멤버 서명).
    let remove = wslog::build_remove(me, v.head_hash, v.head_seq + 1, target, now_ms());
    let remove_bytes = wslog::entry_bytes(&remove);

    // 2) 제거 후 roster → member_set_hash.
    let mut log2 = ctx.log.clone();
    log2.push(remove_bytes.clone());
    let v2 = wslog::verify_chain(&log2, 0).map_err(|e| e.to_string())?;
    let msh = v2.member_set_hash();
    let new_epoch = v.epoch + 1;

    // 3) 새 GK(PCS: 파생 아님, 무작위 신규).
    let new_gk = sb_crypto::GroupKey::generate(new_epoch);

    // 4) Epoch(회전자 서명, reason=Revoke → grace 0).
    let epoch_e = wslog::build_epoch(
        me,
        v2.head_hash,
        v2.head_seq + 1,
        new_epoch,
        sb_proto::EpochReason::Revoke(target),
        msh,
        [0u8; 32],
        now_ms(),
    );
    let epoch_bytes = wslog::entry_bytes(&epoch_e);
    let epoch_hash = wslog::entry_hash(&epoch_bytes);

    // 5) 서명된 RotationBlob(수신자 무관) — 채택될 Epoch 엔트리 해시에 바인딩(§4.4).
    let blob = sb_crypto::build_signed_rotation(
        me,
        new_epoch,
        *new_gk.expose(),
        sb_proto::EpochReason::Revoke(target),
        epoch_hash,
        msh,
    );

    // 6) 남은 멤버별 봉인(자기 자신 제외 — 대상은 이미 v2.roster 밖이라 구조적으로 제외됨).
    let me_id = me.device_id();
    let mut updates = Vec::new();
    for (did, mi) in &v2.members {
        if *did == me_id {
            continue;
        }
        let wrap = sb_crypto::seal_rotation(&mi.kem_pk, &ctx.wid, did, &blob).map_err(|e| e.to_string())?;
        updates.push(sb_proto::KeyUpdate {
            to: *did,
            epoch: new_epoch,
            wrap,
        });
    }

    // ── 송신(순서 중요: Remove → Epoch → KeyUpdate. Epoch 를 KeyUpdate 보다 먼저 보내야
    //    수신자의 verify_rotation(new_epoch==log.epoch) 이 통과한다). ──
    ctx.out
        .send(C2s::AppendEntry {
            entry: remove_bytes.clone(),
        })
        .await
        .map_err(|_| "전송 실패".to_string())?;
    ctx.out
        .send(C2s::AppendEntry {
            entry: epoch_bytes.clone(),
        })
        .await
        .map_err(|_| "전송 실패".to_string())?;
    if !updates.is_empty() {
        ctx.out
            .send(C2s::PutKeyUpdate { updates })
            .await
            .map_err(|_| "전송 실패".to_string())?;
    }

    // ── 락: 로컬 채택. 서버는 송신자에게 LogAppended echo 를 안 하므로 직접 반영한다.
    //    스냅샷 이후 로그가 변하지 않았을 때만 반영(그 사이 다른 엔트리가 붙었다면 서버가
    //    prev_hash 불일치로 우리 append 를 거부했을 것이므로 어디에서도 회전이 없다). ──
    let mut core = state.lock().await;
    if core.log.len() == ctx.log.len() {
        core.log.push(remove_bytes);
        core.log.push(epoch_bytes);
        let _ = persist_group_key(&core, &new_gk);
        core.engine.set_group_key(new_gk.clone());
        core.current_gk = Some(new_gk);
        let target_hex = hex(&target);
        core.members.retain(|m| m.device_id != target_hex);
    } else {
        tracing::warn!("강퇴 중 로그 변경 감지 — 로컬 반영 생략(재연결 시 서버에서 재수신)");
    }
    emit_status(&app, &core);
    let _ = app.emit("members-changed", ());
    Ok(())
}

/// keystore 에 현재 GK 저장(에폭 + 32B).
pub fn persist_group_key(core: &Core, gk: &sb_crypto::GroupKey) -> Result<(), String> {
    let store = crate::worker::keystore_for(&core.data_dir);
    use sb_store::KeyStore;
    let mut buf = gk.epoch().to_le_bytes().to_vec();
    buf.extend_from_slice(gk.expose());
    store.set("group.key", &buf).map_err(|e| e.to_string())
}
