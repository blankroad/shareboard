//! 백그라운드 워커 — 서버 연결 + 클립보드 양방향 동기화 (§3, §5.4).
//!
//! GK 배달은 서명된 RotationBlob + AAD 바인딩 + verify_rotation(roster·서명·epoch·
//! member_set_hash)으로 검증한다(§4.4). 클립보드 감지는 macOS changeCount / 그 외 폴링.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use sb_clipboard::{ChangeWatcher, ClipContent};
use sb_core::{LocalOutcome, RemoteDecision};
use sb_crypto::hash::sha256;
use sb_crypto::wslog;
use sb_crypto::{verify_chain, GroupKey};
use sb_net::ClientHandle;
use sb_proto::params::{CHUNK_SIZE, READ_HARD_LIMIT};
use sb_proto::{C2s, ContentId, ContentKind, DeviceId, LogEntry, Platform, Profile, S2c};
use sb_store::FileKeyStore;

use crate::commands::emit_status;
use crate::core::{hex, AppState, Core, MemberView, PendingAction};

/// 프로필 봉인 AAD(도메인 분리, k_body 재사용).
const PROFILE_AAD: &[u8] = b"sb/profile-v1";

/// OS 로그인 사용자 이름.
fn os_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .filter(|s| !s.trim().is_empty())
}

/// 이 기기의 표시 이름 — 설정 override > OS 사용자 이름 > 비식별 기본값.
fn device_name(core: &Core) -> String {
    if let Some(n) = &core.settings.app.device_name_override {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    os_username().unwrap_or_else(|| {
        let plat = match std::env::consts::OS {
            "macos" => "mac",
            "windows" => "win",
            "linux" => "linux",
            o => o,
        };
        format!("{plat}-{}", &hex(&core.device_id())[..4])
    })
}

fn this_platform() -> Platform {
    match std::env::consts::OS {
        "windows" => Platform::Windows,
        "linux" => Platform::Linux,
        _ => Platform::Macos,
    }
}

/// GK 로 봉인한 enc_profile 에서 이름을 복호(실패 시 None).
fn decode_profile_name(gk: Option<&GroupKey>, enc: &Option<Vec<u8>>) -> Option<String> {
    let gk = gk?;
    let enc = enc.as_ref()?;
    let plain = gk.open_body(PROFILE_AAD, enc).ok()?;
    let p: Profile = sb_proto::decode(&plain).ok()?;
    if p.name.is_empty() {
        None
    } else {
        Some(p.name)
    }
}

/// 내 프로필(이름)을 GK 로 봉인해 SetProfile 로 발행. GK 없으면 no-op.
pub async fn send_profile(state: &AppState) {
    let (out, gk, name, head) = {
        let c = state.lock().await;
        let (Some(out), Some(gk)) = (c.out.clone(), c.current_gk.clone()) else {
            return;
        };
        (out, gk, device_name(&c), c.log_head().1)
    };
    let profile = Profile {
        name,
        platform: this_platform(),
        log_head_hash: head,
        seq: now_ms(),
        epoch: gk.epoch(),
        ts_ms: now_ms(),
    };
    let Ok(bytes) = sb_proto::encode(&profile) else {
        return;
    };
    if let Ok(enc) = gk.seal_body(PROFILE_AAD, &bytes) {
        let _ = out
            .send(C2s::SetProfile {
                epoch: gk.epoch(),
                e2e: enc,
            })
            .await;
    }
}

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
    let listener = tokio::net::TcpListener::bind(bind_addr).await.map_err(|e| {
        // 누가 잡고 있는지까지 알려준다 — 대개 종료되지 않은 이전 인스턴스다.
        match port_holder(port) {
            Some(who) => anyhow::anyhow!(
                "포트 {port} 을 {who} 가 이미 쓰고 있습니다. 이전 shareboard 가 남아 있으면 \
                 트레이 메뉴에서 완전히 종료(창 닫기는 숨김일 뿐)한 뒤 다시 시도하세요"
            ),
            None => anyhow::anyhow!("포트 {port} 바인딩 실패(이미 사용 중일 수 있음): {e}"),
        }
    })?;
    let shared = sb_server::Shared::with_persistence(token_hash, data_dir.join("server"));
    let task = tokio::spawn(sb_server::serve(listener, acceptor, shared));
    tracing::info!("내장 서버 시작 @ {bind}");

    let mut c = state.lock().await;
    c.server_task = Some(task);
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

/// 내장 서버를 중지하고 **포트를 반납**한다. 호스팅 중이 아니었으면 false.
///
/// accept 루프를 abort 하면 리스너가 drop 되어 포트가 즉시 풀린다. 이미 맺어진 연결을 처리하는
/// 하위 태스크는 살아 있을 수 있지만(리스너를 들고 있지 않다) 포트 재바인딩을 막지는 않는다.
pub async fn stop_embedded_server(state: &AppState) -> bool {
    let mut c = state.lock().await;
    let was = c.hosting;
    if let Some(task) = c.server_task.take() {
        task.abort();
    }
    c.hosting = false;
    c.host_addr = None;
    c.host_fp = None;
    // 재시작 시 자동 재호스팅되지 않게 설정에서도 내린다 — 안 내리면 다음 실행에서 또 포트를 잡는다.
    c.settings.server.host = false;
    let _ = sb_store::files::save_json(&c.data_dir.join("settings.json"), &c.settings);
    if was {
        tracing::info!("내장 서버 중지 — 포트 반납");
    }
    was
}

/// 그 포트를 잡고 있는 프로세스 설명(`PID 1234 (shareboard)`). 조회 실패하면 None.
/// 진단 전용 — 외부 통신은 없고 로컬 프로세스만 들여다본다.
fn port_holder(port: u16) -> Option<String> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("/usr/sbin/lsof")
            .args(["-nP", "-sTCP:LISTEN", "-t", &format!("-iTCP:{port}")])
            .output()
            .ok()?;
        let pid = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()?
            .to_string();
        let name = std::process::Command::new("/bin/ps")
            .args(["-o", "comm=", "-p", &pid])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        Some(match name {
            Some(n) => format!("PID {pid} ({n})"),
            None => format!("PID {pid}"),
        })
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("netstat")
            .args(["-ano", "-p", "tcp"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let needle = format!(":{port}");
        let pid = text
            .lines()
            .filter(|l| l.contains("LISTENING") && l.contains(&needle))
            .filter_map(|l| l.split_whitespace().last())
            .next()?
            .to_string();
        Some(format!("PID {pid}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = port;
        None
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 클립보드 워처 공유 핸들 — 로컬 감시 태스크와 원격 적용(write) 경로가 같은 워처를 쓴다.
/// (macOS changeCount 상태를 공유해야 하고, 워처를 두 개 두면 서로의 write 를 새 클립으로 오인한다.)
pub type SharedWatcher = Arc<Mutex<Box<dyn ChangeWatcher + Send>>>;

/// 로컬 클립보드 감시 — **연결 여부와 무관하게** 앱 수명 동안 돈다.
/// 히스토리는 오프라인·동기화 off 에서도 쌓여야 하고(팝업이 그걸 쓴다), 연결돼 있으면 신호까지 발행한다.
async fn clipboard_loop(app: AppHandle, state: AppState, watcher: SharedWatcher) {
    let mut ticker = tokio::time::interval(Duration::from_millis(400));
    loop {
        ticker.tick().await;
        poll_clipboard(&app, &state, &watcher).await;
    }
}

/// 워커 메인 — 재연결 루프.
pub async fn run(app: AppHandle, state: AppState) {
    let watcher: SharedWatcher = Arc::new(Mutex::new(sb_clipboard::system_watcher()));
    tauri::async_runtime::spawn(clipboard_loop(app.clone(), state.clone(), watcher.clone()));

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
                session(&app, &state, &watcher, handle).await;
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
async fn session(app: &AppHandle, state: &AppState, watcher: &SharedWatcher, mut handle: ClientHandle) {
    // 온보딩 대기 작업.
    let pending = { state.lock().await.pending.clone() };
    match pending {
        Some(PendingAction::Claim { genesis, token }) => {
            if let Err(e) = claim_workspace(state, &mut handle, genesis, token).await {
                tracing::warn!("워크스페이스 생성 실패: {e}");
                fail_onboarding(app, state, e, true).await;
                return;
            }
        }
        Some(PendingAction::Join { code }) => {
            if let Err(e) = guest_join(state, &mut handle, &code).await {
                tracing::warn!("조인 실패: {e}");
                fail_onboarding(app, state, e, false).await;
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
    // 내 기기 이름을 다른 멤버에게 알림(GK 있으면).
    send_profile(state).await;

    // 수신 루프. 로컬 클립보드 감시는 연결과 무관하게 clipboard_loop 가 담당한다.
    let reconnect = { state.lock().await.reconnect.clone() };
    let mut fetches: HashMap<ContentId, (u64, Vec<u8>)> = HashMap::new();

    loop {
        tokio::select! {
            msg = handle.inbox.recv() => {
                match msg {
                    Some(m) => {
                        if !handle_msg(app, state, watcher, &mut fetches, &handle, m).await {
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

/// 클립을 건너뛴 이유를 사용자에게 알린다 — 조용히 사라지면 "복사했는데 아무 일도 없다"가 된다.
fn notify_skip(app: &AppHandle, why: String) {
    tracing::warn!("클립 건너뜀: {why}");
    let _ = app.emit("clip-skipped", why);
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.0)
}

/// 로컬 클립보드 변경 → 신호 발행.
async fn poll_clipboard(app: &AppHandle, state: &AppState, watcher: &SharedWatcher) {
    // 워처 락은 여기서 놓는다 — 코어 락과 겹치지 않게(교착 방지).
    let (content, concealed) = {
        let mut w = watcher.lock().await;
        match w.poll() {
            Ok(Some(c)) => {
                let concealed = w.is_concealed();
                (c, concealed)
            }
            Ok(None) => return,
            // 크기 상한·폴더 등 정책상 건너뜀 → 이유를 UI 로 올린다.
            Err(sb_clipboard::ClipError::Skipped(why)) => return notify_skip(app, why),
            Err(e) => {
                tracing::debug!("클립보드 읽기 실패: {e}");
                return;
            }
        }
    };
    if content.bytes.len() as u64 > READ_HARD_LIMIT {
        tracing::warn!("클립보드가 READ_HARD_LIMIT 초과 — 스킵");
        return;
    }
    let mut core = state.lock().await;
    if core.settings.privacy.exclude_concealed && concealed {
        // concealed(비밀번호 매니저) 제외 (§4.6) — 왜 사라졌는지 알려준다.
        drop(core);
        return notify_skip(
            app,
            "비밀번호 매니저 등이 '민감 콘텐츠'로 표시한 클립이라 제외했습니다 (설정 → 히스토리/개인정보)"
                .into(),
        );
    }
    let outcome = core
        .engine
        .on_local_clipboard(content.kind, &content.bytes, now_ms());
    tracing::debug!(
        "로컬 클립 감지: {:?} {}B (본문 캐시 {}) → {}",
        content.kind,
        content.bytes.len(),
        mb(core.body_cache_bytes()),
        match &outcome {
            Ok(LocalOutcome::Echo) => "에코(무시)",
            Ok(LocalOutcome::Emit(_)) if core.out.is_some() => "히스토리 + 발행",
            Ok(LocalOutcome::Emit(_)) => "히스토리 (미연결 — 발행 안 함)",
            Ok(_) => "히스토리만",
            Err(_) => "히스토리만 (봉인 실패)",
        }
    );
    match outcome {
        // 에코(원격 write 되돌아옴)는 히스토리에도 남지 않는다 — 할 일 없음.
        Ok(LocalOutcome::Echo) => {}
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
        // 동기화 off·kind off·상한 초과, 그리고 봉인 실패까지 — 엔진은 이미 로컬 히스토리에
        // 넣었다. 본문 캐시와 UI 갱신을 빠뜨리면 항목이 "복사 불가"로 남고 목록도 늦게 뜬다.
        _ => {
            // 왜 안 보내는지 알려준다(특히 파일: 상한을 올리면 되는 경우가 많다).
            let limit = core.settings.sync.max_content_bytes;
            match &outcome {
                Ok(LocalOutcome::TooLarge) => notify_skip(
                    app,
                    format!(
                        "{} — {}(상한 {})를 넘어 다른 기기로 보내지 않았습니다. 설정 → 동기화에서 최대 크기를 올리세요",
                        if content.kind == ContentKind::Files { "파일" } else { "콘텐츠" },
                        mb(content.bytes.len() as u64),
                        mb(limit)
                    ),
                ),
                Ok(LocalOutcome::KindDisabled) => notify_skip(
                    app,
                    match content.kind {
                        ContentKind::Files => "파일 동기화가 꺼져 있습니다 (설정 → 동기화)".into(),
                        ContentKind::ImagePng => "이미지 동기화가 꺼져 있습니다 (설정 → 동기화)".into(),
                        ContentKind::Text => "텍스트 동기화가 꺼져 있습니다 (설정 → 동기화)".to_string(),
                    },
                ),
                _ => {}
            }
            let newest = core.engine.history().list().next().map(|i| i.id);
            if let Some(id) = newest {
                core.cache_body(id, content.bytes.clone());
            }
            let _ = app.emit("history-updated", ());
        }
    }
}

/// 서버 메시지 처리. false = 세션 종료.
async fn handle_msg(
    app: &AppHandle,
    state: &AppState,
    watcher: &SharedWatcher,
    fetches: &mut HashMap<ContentId, (u64, Vec<u8>)>,
    handle: &ClientHandle,
    msg: S2c,
) -> bool {
    match msg {
        S2c::Welcome(w) => {
            let mut core = state.lock().await;
            // 1) 로그 먼저 반영 — 회전 wrap 검증(verify_rotation)은 최신 로그(epoch·roster)
            //    기준이어야 한다. 오프라인 중 강퇴 회전을 받은 남은 멤버가 밀린 wrap 을
            //    stale 로그로 검증하면 epoch/member_set_hash 불일치로 영영 채택 못 함.
            if !w.log_tail.is_empty() {
                core.log = w.log_tail.clone();
            }
            // 2) 밀린 wrap 적용(이제 최신 로그에 대해 검증 통과 — presence 프로필 복호에도 필요).
            if let Some(wrapped) = w.pending_key_update.clone() {
                try_set_gk_from_wrap(&mut core, &wrapped);
            }
            // 3) 로그 기반 상태/멤버 갱신(GK 확보 후 프로필 복호).
            //
            // log_tail 이 비어 있어도(이미 최신) **presence 는 반영해야 한다** — 서버 재시작 후
            // 재연결처럼 tail 이 없는 경우에 목록이 옛 online 상태로 굳는다.
            if let Ok(v) = verify_chain(&core.log, 0) {
                core.workspace_id = Some(v.workspace_id);
                core.settings.server.workspace_name = Some(v.workspace_name.clone());
                let gk = core.current_gk.clone();
                core.members = members_from(&v, &w.presence, gk.as_ref());
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
            // 내 프로필 발행(Welcome 으로 GK 확보했을 수 있음).
            send_profile(state).await;
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
            device_id,
            online,
            addr,
            enc_profile,
        } => {
            let mut core = state.lock().await;
            let name = decode_profile_name(core.current_gk.as_ref(), &enc_profile);
            let id_hex = hex(&device_id);
            if let Some(m) = core.members.iter_mut().find(|m| m.device_id == id_hex) {
                m.online = online;
                if addr.is_some() {
                    m.addr = addr;
                }
                if name.is_some() {
                    m.name = name;
                }
            } else if verify_chain(&core.log, 0)
                .map(|v| v.is_member(&device_id))
                .unwrap_or(false)
            {
                // 로그상 멤버인데 목록에 없다 = presence 가 LogAppended 보다 먼저 왔다.
                // 버리면 "상대는 연결됐는데 내 목록엔 오프라인"이 영구히 남는다.
                core.members.push(MemberView {
                    device_id: id_hex,
                    name,
                    online,
                    platform: String::new(),
                    addr,
                });
            } else {
                tracing::debug!("멤버가 아닌 device 의 presence 무시: {}", &id_hex[..8]);
            }
            let _ = app.emit("members-changed", ());
            emit_status(app, &core);
        }
        S2c::LogAppended { entry, .. } => {
            on_log_appended(app, state, handle, entry).await;
        }
        S2c::KeyUpdatePush { wrap } => {
            {
                let mut core = state.lock().await;
                try_set_gk_from_wrap(&mut core, &wrap);
                emit_status(app, &core);
            }
            // GK 확보 후 내 프로필 발행.
            send_profile(state).await;
        }
        S2c::Bye { .. } => return false,
        _ => {}
    }
    true
}

/// 적용된 콘텐츠를 OS 클립보드에 올린다.
///
/// 파일은 바이트를 그대로 쓸 수 없다 — 디스크에 실체화한 뒤 그 **경로**를 클립보드에 올려야
/// Finder/탐색기에서 붙여넣을 수 있다.
async fn apply_to_clipboard(
    app: &AppHandle,
    state: &AppState,
    watcher: &SharedWatcher,
    kind: ContentKind,
    bytes: Vec<u8>,
) {
    if kind == ContentKind::Files {
        let (dir, mark) = {
            let c = state.lock().await;
            (c.data_dir.clone(), c.settings.privacy.mark_received_files)
        };
        let bundle = match sb_clipboard::files::bundle_from_bytes(&bytes) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("받은 파일 번들 해석 실패: {e}");
                return;
            }
        };
        let count = bundle.files.len();
        let paths = match crate::received::materialize(&dir, &bundle, mark) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("받은 파일 저장 실패: {e}");
                return;
            }
        };
        tracing::debug!("파일 {count}개 저장 → 클립보드에 경로 올림");
        if let Err(e) = watcher.lock().await.write_file_paths(&paths) {
            tracing::warn!("파일 클립보드 쓰기 실패: {e}");
        }
    } else {
        let content = ClipContent { kind, bytes };
        let _ = watcher.lock().await.write(&content);
    }
    let _ = app.emit("clipboard-synced", ());
    let _ = app.emit("history-updated", ());
}

/// 신호 → LWW 판정 → inline 적용 또는 fetch 요청.
async fn apply_signal(
    app: &AppHandle,
    state: &AppState,
    watcher: &SharedWatcher,
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
            apply_to_clipboard(app, state, watcher, kind, plaintext).await;
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
    watcher: &SharedWatcher,
    id: ContentId,
    sealed: &[u8],
) {
    let mut core = state.lock().await;
    match core.engine.on_content_fetched(id, sealed, now_ms()) {
        Ok(applied) => {
            core.cache_body(applied.id, applied.plaintext.clone());
            drop(core);
            apply_to_clipboard(app, state, watcher, applied.kind, applied.plaintext).await;
        }
        Err(e) => tracing::warn!("fetch 적용 실패: {e}"),
    }
}

/// 새 Add 를 보면 조인자에게 GK wrap 배달.
async fn on_log_appended(app: &AppHandle, state: &AppState, handle: &ClientHandle, entry: Vec<u8>) {
    let mut core = state.lock().await;
    core.log.push(entry.clone());
    let Ok(v) = verify_chain(&core.log, 0) else { return };
    // 기존 멤버의 presence(online/addr/name)를 보존하고 새 멤버만 추가.
    let prev: std::collections::HashMap<String, MemberView> = core
        .members
        .iter()
        .map(|m| (m.device_id.clone(), m.clone()))
        .collect();
    core.members = v
        .members
        .keys()
        .map(|d| {
            let id = hex(d);
            prev.get(&id).cloned().unwrap_or(MemberView {
                device_id: id,
                name: None,
                online: false,
                platform: String::new(),
                addr: None,
            })
        })
        .collect();
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

/// 온보딩(생성/조인) 실패 처리 — 사유를 UI 에 남기고 서버 설정을 지워 온보딩으로 되돌린다.
/// pending 도 해제해 같은 실패를 무한 재시도하지 않는다.
///
/// `drop_workspace` 는 **생성 실패** 전용이다. 창립 시 만든 genesis·GK 는 서버가 받아들이기
/// 전까지 순수 로컬 상태이므로, 클레임이 거부되면 그대로 두면 안 된다(서버 로그와 어긋난
/// 유령 워크스페이스가 된다). keystore 의 group.key 는 다음 생성/조인 때 덮어써지므로 남겨둔다.
async fn fail_onboarding(app: &AppHandle, state: &AppState, reason: String, drop_workspace: bool) {
    let mut c = state.lock().await;
    c.join_error = Some(reason);
    c.joining = false;
    c.pending = None;
    if drop_workspace {
        c.log.clear();
        c.workspace_id = None;
        c.current_gk = None;
        c.gk_present = false;
    }
    c.settings.server.addr = None;
    c.settings.server.fingerprint_hex = None;
    c.settings.server.workspace_name = None;
    c.server_fp = None;
    let _ = sb_store::files::save_json(&c.data_dir.join("settings.json"), &c.settings);
    emit_status(app, &c);
}

/// 창립자 클레임 (§4.3.2) — ClaimWorkspace 를 보내고 **응답을 확인**한다.
///
/// 서버는 토큰 불일치·이미 클레임됨·genesis 무효를 전부 `Error` 로 답한다. 응답을 버리면
/// 서버가 거부한 워크스페이스를 앱이 성공으로 착각해(로컬 GK·genesis 보유) 아무도 참여할 수
/// 없는 상태가 된다.
async fn claim_workspace(
    state: &AppState,
    handle: &mut ClientHandle,
    genesis: Vec<u8>,
    token: String,
) -> Result<(), String> {
    handle
        .out
        .send(C2s::ClaimWorkspace { token, genesis })
        .await
        .map_err(|_| "전송 실패".to_string())?;
    recv_claim_ack(handle).await?;
    // 성공 즉시 pending 해제 — 이 세션이 Hello 전에 끊겨도 재연결 때 다시 클레임을 보내
    // "이미 클레임됨" 으로 정상 워크스페이스를 실패 처리하는 일이 없도록 한다.
    state.lock().await.pending = None;
    Ok(())
}

/// ClaimWorkspace 응답 — AppendAck = 성공, Error = 실패 사유(서버 문구 그대로 UI 로).
async fn recv_claim_ack(handle: &mut ClientHandle) -> Result<(), String> {
    for _ in 0..50 {
        match handle.inbox.recv().await {
            Some(S2c::AppendAck { .. }) => return Ok(()),
            Some(S2c::Error { detail, .. }) => return Err(detail),
            Some(_) => continue,
            None => return Err("서버 연결이 끊겼습니다".into()),
        }
    }
    Err("서버 응답 없음".into())
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

/// VerifiedLog + presence → MemberView 목록. `gk` 로 enc_profile 의 이름을 복호.
fn members_from(
    v: &sb_crypto::VerifiedLog,
    presence: &[sb_proto::PresenceEntry],
    gk: Option<&GroupKey>,
) -> Vec<MemberView> {
    v.members
        .keys()
        .map(|d| {
            let p = presence.iter().find(|e| &e.device_id == d);
            MemberView {
                device_id: hex(d),
                name: p.and_then(|e| decode_profile_name(gk, &e.enc_profile)),
                online: p.map(|e| e.online).unwrap_or(false),
                platform: String::new(),
                addr: p.and_then(|e| e.addr.clone()),
            }
        })
        .collect()
}
