//! 백그라운드 워커 — 서버 연결 + 클립보드 양방향 동기화 (§3, §5.4).
//!
//! GK 배달은 서명된 RotationBlob + AAD 바인딩 + verify_rotation(roster·서명·epoch·
//! member_set_hash)으로 검증한다(§4.4). 클립보드 감지는 macOS changeCount / 그 외 폴링.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};

use sb_clipboard::{ChangeWatcher, ClipContent};
use sb_core::{LocalOutcome, RemoteDecision};
use sb_crypto::hash::sha256;
use sb_crypto::wslog;
use sb_crypto::{verify_chain, GroupKey};
use sb_net::ClientHandle;
use sb_proto::params::{CHUNK_SIZE, READ_HARD_LIMIT};
use sb_proto::{C2s, ContentId, DeviceId, LogEntry, S2c};
use sb_store::FileKeyStore;

use crate::commands::emit_status;
use crate::core::{hex, AppState, Core, MemberView, PendingAction};

pub fn keystore_for(data_dir: &Path) -> FileKeyStore {
    FileKeyStore::new(data_dir.join("keys")).expect("keystore dir")
}

/// 사내망(사설) IPv4 감지 — 호스팅 바인딩 주소. 없으면 loopback.
pub fn detect_lan_ip() -> std::net::Ipv4Addr {
    if_addrs::get_if_addrs()
        .ok()
        .into_iter()
        .flatten()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) if v4.is_private() => Some(v4),
            _ => None,
        })
        .next()
        .unwrap_or(std::net::Ipv4Addr::LOCALHOST)
}

/// 서버 신원(클라 신원과 별개) get-or-create.
pub fn load_or_create_server_identity(store: &FileKeyStore) -> anyhow::Result<sb_crypto::Identity> {
    use sb_store::KeyStore;
    match (store.get("server.signing")?, store.get("server.kem")?) {
        (Some(s), Some(k)) if k.len() == 32 => {
            let mut kem = [0u8; 32];
            kem.copy_from_slice(&k);
            sb_crypto::Identity::from_parts(&s, &kem).map_err(|e| anyhow::anyhow!("{e}"))
        }
        _ => {
            let id = sb_crypto::Identity::generate();
            store.set(
                "server.signing",
                &id.signing_pkcs8_der().map_err(|e| anyhow::anyhow!("{e}"))?,
            )?;
            store.set("server.kem", &id.kem_secret_bytes())?;
            Ok(id)
        }
    }
}

/// 내장 릴레이 서버 시작(이미 실행 중이면 기존 정보 반환). 반환 `(bind_addr, fingerprint)`.
pub async fn start_embedded_server(
    state: &AppState,
    token_hash: Option<[u8; 32]>,
) -> anyhow::Result<(String, [u8; 32])> {
    let (data_dir, port, already, cur) = {
        let c = state.lock().await;
        (
            c.data_dir.clone(),
            c.settings
                .server
                .host_port
                .unwrap_or(sb_proto::params::DEFAULT_SERVER_PORT),
            c.hosting,
            (c.host_addr.clone(), c.host_fp),
        )
    };
    if already {
        return Ok((cur.0.unwrap_or_default(), cur.1.unwrap_or([0u8; 32])));
    }

    let store = keystore_for(&data_dir);
    let server_id = load_or_create_server_identity(&store)?;
    let server_fp = server_id.device_id();
    let bind = format!("{}:{}", detect_lan_ip(), port);
    let bind_addr: SocketAddr = bind.parse()?;
    let acceptor = tokio_rustls::TlsAcceptor::from(
        sb_server::tls::server_config(&server_id).map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| anyhow::anyhow!("포트 {port} 바인딩 실패(이미 사용 중일 수 있음): {e}"))?;
    let shared = sb_server::Shared::with_persistence(token_hash, data_dir.join("server"));
    tokio::spawn(sb_server::serve(listener, acceptor, shared));
    tracing::info!("내장 서버 시작 @ {bind}");

    let mut c = state.lock().await;
    c.hosting = true;
    c.host_addr = Some(bind.clone());
    c.host_fp = Some(server_fp);
    c.server_fp = Some(server_fp);
    c.settings.server.addr = Some(bind.clone());
    c.settings.server.fingerprint_hex = Some(hex(&server_fp));
    c.settings.server.host = true;
    c.settings.server.host_port = Some(port);
    let _ = sb_store::files::save_json(&c.data_dir.join("settings.json"), &c.settings);
    Ok((bind, server_fp))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 워커 메인 — 재연결 루프.
pub async fn run(app: AppHandle, state: AppState) {
    // 재시작 시 호스팅 자동 재개(설정에 host=true 인 경우).
    let host_on_start = { state.lock().await.settings.server.host };
    if host_on_start {
        if let Err(e) = start_embedded_server(&state, None).await {
            tracing::error!("내장 서버 재호스팅 실패: {e}");
        } else {
            let core = state.lock().await;
            emit_status(&app, &core);
        }
    }

    loop {
        let (addr_opt, fp_opt, identity, reconnect) = {
            let core = state.lock().await;
            (
                core.server_addr(),
                core.server_fp,
                core.identity.clone(),
                core.reconnect.clone(),
            )
        };

        let (Some(addr_str), Some(fp)) = (addr_opt, fp_opt) else {
            reconnect.notified().await;
            continue;
        };
        let Ok(addr) = addr_str.parse::<SocketAddr>() else {
            reconnect.notified().await;
            continue;
        };

        match sb_net::connect(addr, fp, &identity).await {
            Ok(conn) => {
                let handle = sb_net::spawn(conn);
                session(&app, &state, handle).await;
            }
            Err(e) => tracing::warn!("서버 연결 실패: {e}"),
        }

        {
            let mut core = state.lock().await;
            core.connected = false;
            core.out = None;
            emit_status(&app, &core);
        }
        // 백오프 또는 재연결 신호.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            _ = reconnect.notified() => {}
        }
    }
}

/// 한 연결 세션.
async fn session(app: &AppHandle, state: &AppState, mut handle: ClientHandle) {
    // 온보딩 대기 작업.
    let pending = { state.lock().await.pending.clone() };
    match pending {
        Some(PendingAction::Claim { genesis, token }) => {
            let _ = handle.out.send(C2s::ClaimWorkspace { token, genesis }).await;
            // AppendAck 는 아래 루프에서 흡수. 창립자는 곧바로 멤버.
        }
        Some(PendingAction::Join { code }) => {
            if let Err(e) = guest_join(state, &mut handle, &code).await {
                tracing::warn!("조인 실패: {e}");
                // 실패를 UI에 확인시키고 온보딩으로 복귀(재시도 루프 방지).
                let mut c = state.lock().await;
                c.join_error = Some(e);
                c.joining = false;
                c.pending = None;
                c.settings.server.addr = None;
                c.settings.server.fingerprint_hex = None;
                c.settings.server.workspace_name = None;
                c.server_fp = None;
                let _ = sb_store::files::save_json(&c.data_dir.join("settings.json"), &c.settings);
                emit_status(app, &c);
                return;
            }
        }
        None => {}
    }

    // Member Hello.
    let (epoch, log_head, device_id, app_ver) = {
        let c = state.lock().await;
        (
            c.engine.epoch(),
            c.log_head(),
            c.device_id(),
            env!("CARGO_PKG_VERSION").to_string(),
        )
    };
    let _ = handle
        .out
        .send(C2s::Hello(sb_proto::Hello {
            device_id,
            proto_min: sb_proto::params::PROTO_MIN,
            proto_max: sb_proto::params::PROTO_MAX,
            app_version: app_ver,
            epoch,
            log_head,
        }))
        .await;

    {
        let mut core = state.lock().await;
        core.out = Some(handle.out.clone());
        core.connected = true;
        core.pending = None;
        core.joining = false;
        core.join_error = None;
        emit_status(app, &core);
    }

    // 동기화 루프. macOS 는 changeCount 기반, 그 외는 폴링 (sb-clipboard::system_watcher).
    let mut watcher = sb_clipboard::system_watcher();
    let mut ticker = tokio::time::interval(Duration::from_millis(400));
    let reconnect = { state.lock().await.reconnect.clone() };
    let mut fetches: HashMap<ContentId, (u64, Vec<u8>)> = HashMap::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                poll_clipboard(app, state, &mut *watcher).await;
            }
            msg = handle.inbox.recv() => {
                match msg {
                    Some(m) => {
                        if !handle_msg(app, state, &mut *watcher, &mut fetches, &handle, m).await {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = reconnect.notified() => break,
        }
    }
}

/// 로컬 클립보드 변경 → 신호 발행.
async fn poll_clipboard(app: &AppHandle, state: &AppState, watcher: &mut (dyn ChangeWatcher + Send)) {
    let content = match watcher.poll() {
        Ok(Some(c)) => c,
        _ => return,
    };
    if content.bytes.len() as u64 > READ_HARD_LIMIT {
        tracing::warn!("클립보드가 READ_HARD_LIMIT 초과 — 스킵");
        return;
    }
    let mut core = state.lock().await;
    if core.settings.privacy.exclude_concealed && watcher.is_concealed() {
        return; // concealed(비밀번호 매니저) 제외 (§4.6)
    }
    match core
        .engine
        .on_local_clipboard(content.kind, &content.bytes, now_ms())
    {
        Ok(LocalOutcome::Emit(sig)) => {
            core.cache_body(sig.hdr.id, content.bytes.clone());
            if let Some(out) = &core.out {
                let _ = out
                    .send(C2s::ClipSignal {
                        hdr: sig.hdr.clone(),
                        e2e: sig.e2e.clone(),
                    })
                    .await;
            }
            let _ = app.emit("history-updated", ());
        }
        _ => {}
    }
}

/// 서버 메시지 처리. false = 세션 종료.
async fn handle_msg(
    app: &AppHandle,
    state: &AppState,
    watcher: &mut (dyn ChangeWatcher + Send),
    fetches: &mut HashMap<ContentId, (u64, Vec<u8>)>,
    handle: &ClientHandle,
    msg: S2c,
) -> bool {
    match msg {
        S2c::Welcome(w) => {
            let mut core = state.lock().await;
            // 로그 반영(있으면).
            if !w.log_tail.is_empty() {
                core.log = w.log_tail.clone();
                if let Ok(v) = verify_chain(&core.log, 0) {
                    core.workspace_id = Some(v.workspace_id);
                    core.settings.server.workspace_name = Some(v.workspace_name.clone());
                    core.members = members_from(&v, &w.presence);
                }
            }
            // 밀린 wrap.
            if let Some(wrapped) = w.pending_key_update.clone() {
                try_set_gk_from_wrap(&mut core, &wrapped);
            }
            core.connected = true;
            emit_status(app, &core);
            // head 적용(inline만 즉시).
            let signals = w.head.clone();
            drop(core);
            for (origin, hdr, e2e) in signals {
                apply_signal(app, state, watcher, handle, origin, hdr, e2e).await;
            }
            let _ = app.emit("members-changed", ());
        }
        S2c::SignalFanout { origin, hdr, e2e } => {
            apply_signal(app, state, watcher, handle, origin, hdr, e2e).await;
        }
        S2c::ContentPull { id } => {
            // 우리가 origin — 본문 서빙.
            let core = state.lock().await;
            if let Some(ct) = core.engine.serve_content(&id, now_ms()) {
                if let Some(out) = &core.out {
                    let chunks = ct.chunks(CHUNK_SIZE).count() as u32;
                    let _ = out
                        .send(C2s::ContentBegin {
                            id,
                            ct_size: ct.len() as u64,
                            chunk_count: chunks,
                            chunk_size: CHUNK_SIZE as u32,
                        })
                        .await;
                    for (i, ch) in ct.chunks(CHUNK_SIZE).enumerate() {
                        let _ = out
                            .send(C2s::ContentChunk {
                                id,
                                index: i as u32,
                                data: ch.to_vec(),
                            })
                            .await;
                    }
                }
            } else if let Some(out) = &core.out {
                let _ = out
                    .send(C2s::ContentReject {
                        id,
                        reason: sb_proto::RejectReason::Gone,
                    })
                    .await;
            }
        }
        S2c::ContentBegin { id, ct_size, .. } => {
            fetches.insert(id, (ct_size, Vec::with_capacity(ct_size as usize)));
        }
        S2c::ContentChunk { id, data, .. } => {
            if let Some((ct_size, buf)) = fetches.get_mut(&id) {
                buf.extend_from_slice(&data);
                if buf.len() as u64 >= *ct_size {
                    let (_sz, full) = fetches.remove(&id).unwrap();
                    finish_fetch(app, state, watcher, id, &full).await;
                }
            }
        }
        S2c::ContentReject { id, .. } | S2c::ContentAbort { id, .. } => {
            fetches.remove(&id);
        }
        S2c::Presence {
            device_id, online, ..
        } => {
            let mut core = state.lock().await;
            if let Some(m) = core.members.iter_mut().find(|m| m.device_id == hex(&device_id)) {
                m.online = online;
            }
            let _ = app.emit("members-changed", ());
            emit_status(app, &core);
        }
        S2c::LogAppended { entry, .. } => {
            on_log_appended(app, state, handle, entry).await;
        }
        S2c::KeyUpdatePush { wrap } => {
            let mut core = state.lock().await;
            try_set_gk_from_wrap(&mut core, &wrap);
            emit_status(app, &core);
        }
        S2c::Bye { .. } => return false,
        _ => {}
    }
    true
}

/// 신호 → LWW 판정 → inline 적용 또는 fetch 요청.
async fn apply_signal(
    app: &AppHandle,
    state: &AppState,
    watcher: &mut (dyn ChangeWatcher + Send),
    handle: &ClientHandle,
    origin: DeviceId,
    hdr: sb_proto::SignalHdr,
    e2e: Vec<u8>,
) {
    let mut core = state.lock().await;
    let epoch = hdr.epoch;
    match core.engine.on_remote_signal(origin, hdr.clone(), &e2e, now_ms()) {
        RemoteDecision::ApplyInline { id, kind, plaintext } => {
            core.cache_body(id, plaintext.clone());
            drop(core);
            let content = ClipContent {
                kind,
                bytes: plaintext,
            };
            let _ = watcher.write(&content);
            let _ = app.emit("clipboard-synced", ());
            let _ = app.emit("history-updated", ());
        }
        RemoteDecision::NeedFetch { id, .. } => {
            let _ = handle.out.send(C2s::ContentRequest { id, epoch }).await;
        }
        RemoteDecision::Ignore(_) => {}
    }
}

async fn finish_fetch(
    app: &AppHandle,
    state: &AppState,
    watcher: &mut (dyn ChangeWatcher + Send),
    id: ContentId,
    sealed: &[u8],
) {
    let mut core = state.lock().await;
    match core.engine.on_content_fetched(id, sealed, now_ms()) {
        Ok(applied) => {
            core.cache_body(applied.id, applied.plaintext.clone());
            drop(core);
            let content = ClipContent {
                kind: applied.kind,
                bytes: applied.plaintext,
            };
            let _ = watcher.write(&content);
            let _ = app.emit("clipboard-synced", ());
            let _ = app.emit("history-updated", ());
        }
        Err(e) => tracing::warn!("fetch 적용 실패: {e}"),
    }
}

/// 새 Add 를 보면 조인자에게 GK wrap 배달.
async fn on_log_appended(app: &AppHandle, state: &AppState, handle: &ClientHandle, entry: Vec<u8>) {
    let mut core = state.lock().await;
    core.log.push(entry.clone());
    let Ok(v) = verify_chain(&core.log, 0) else { return };
    core.members = members_from(&v, &[]);
    let _ = app.emit("members-changed", ());

    if let Ok(LogEntry::Add {
        subject_spki,
        subject_kem_pk,
        ..
    }) = sb_proto::decode::<LogEntry>(&entry)
    {
        let subject = sha256(&subject_spki);
        let (Some(gk), Some(wid)) = (core.current_gk.clone(), core.workspace_id) else {
            return;
        };
        if subject == core.device_id() {
            return;
        }
        // 서명된 RotationBlob + AAD 바인딩(§4.4) — 조인자가 verify_rotation 으로 검증.
        let identity = core.identity.clone();
        let blob = sb_crypto::build_signed_rotation(
            &identity,
            gk.epoch(),
            *gk.expose(),
            sb_proto::EpochReason::Join,
            v.head_hash,
            v.member_set_hash(),
        );
        if let Ok(wrapped) = sb_crypto::seal_rotation(&subject_kem_pk, &wid, &subject, &blob) {
            let _ = handle
                .out
                .send(C2s::PutKeyUpdate {
                    updates: vec![sb_proto::KeyUpdate {
                        to: subject,
                        epoch: gk.epoch(),
                        wrap: wrapped,
                    }],
                })
                .await;
        }
    }
}

/// wrap 을 열고 **검증**한 뒤 GK 설정 (§4.4 — 서명·roster·epoch·member_set_hash).
fn try_set_gk_from_wrap(core: &mut Core, wrapped: &[u8]) {
    let Some(wid) = core.workspace_id else { return };
    let identity = core.identity.clone();
    let Ok(blob) = sb_crypto::open_rotation(&identity, &wid, wrapped) else {
        return;
    };
    // 검증된 로그에 대해 wrap 검증(비멤버·서버 주입·roster 밖 키 차단).
    let Ok(vlog) = verify_chain(&core.log, 0) else {
        return;
    };
    if !sb_crypto::verify_rotation(&blob, &vlog) {
        tracing::warn!("GK wrap 검증 실패 — 무시(서버 주입 의심)");
        return;
    }
    let gk = GroupKey::from_bytes(blob.new_epoch, blob.group_key);
    let _ = crate::commands::persist_group_key(core, &gk);
    core.engine.set_group_key(gk.clone());
    core.current_gk = Some(gk);
    core.gk_present = true;
}

/// 게스트 조인 플로우 (§4.3.4). 같은 연결에서 Add 후 멤버로 승격.
async fn guest_join(state: &AppState, handle: &mut ClientHandle, code: &str) -> Result<(), String> {
    // 1. 로그 취득.
    handle
        .out
        .send(C2s::GetLog { from_seq: 0 })
        .await
        .map_err(|_| "send")?;
    let entries = recv_log(handle).await.ok_or("로그 수신 실패")?;
    let v = verify_chain(&entries, 0).map_err(|e| e.to_string())?;
    let wid = v.workspace_id;

    // 2. 초대 blob.
    let (locator, _k) = sb_crypto::invite::derive(code, &wid).map_err(|e| e.to_string())?;
    handle
        .out
        .send(C2s::GetInviteBlob { locator })
        .await
        .map_err(|_| "send")?;
    let blob = recv_invite(handle)
        .await
        .ok_or("초대 blob 없음(코드 오류/만료)")?;

    // 3. Add 작성 → append.
    let joiner = { state.lock().await.identity.public() };
    let (add, secret) =
        sb_crypto::build_add_from_blob(code, wid, &blob, &joiner, v.head_hash, v.head_seq + 1, now_ms())
            .map_err(|e| e.to_string())?;
    let add_bytes = wslog::entry_bytes(&add);
    handle
        .out
        .send(C2s::AppendEntry {
            entry: add_bytes.clone(),
        })
        .await
        .map_err(|_| "send")?;
    let _ = recv_ack(handle).await;

    // 4. 로컬 상태 반영.
    let mut core = state.lock().await;
    core.log = entries;
    core.log.push(add_bytes);
    core.workspace_id = Some(wid);
    core.server_fp = Some(secret.server_fp);
    core.settings.server.fingerprint_hex = Some(hex(&secret.server_fp));
    core.settings.server.workspace_name = Some(v.workspace_name);
    Ok(())
}

async fn recv_log(handle: &mut ClientHandle) -> Option<Vec<Vec<u8>>> {
    for _ in 0..50 {
        match handle.inbox.recv().await? {
            S2c::LogEntries { entries, .. } => return Some(entries),
            _ => continue,
        }
    }
    None
}
async fn recv_invite(handle: &mut ClientHandle) -> Option<Vec<u8>> {
    for _ in 0..50 {
        match handle.inbox.recv().await? {
            S2c::InviteBlob { blob } => return blob,
            _ => continue,
        }
    }
    None
}
async fn recv_ack(handle: &mut ClientHandle) -> Option<u64> {
    for _ in 0..50 {
        match handle.inbox.recv().await? {
            S2c::AppendAck { seq, .. } => return Some(seq),
            S2c::AppendReject { .. } => return None,
            _ => continue,
        }
    }
    None
}

/// VerifiedLog + presence → MemberView 목록.
fn members_from(
    v: &sb_crypto::VerifiedLog,
    presence: &[(DeviceId, bool, Option<Vec<u8>>)],
) -> Vec<MemberView> {
    v.members
        .keys()
        .map(|d| {
            let online = presence.iter().any(|(pd, on, _)| pd == d && *on);
            MemberView {
                device_id: hex(d),
                name: crate::core::hex(d)[..8].to_string(),
                online,
                platform: String::new(),
            }
        })
        .collect()
}
