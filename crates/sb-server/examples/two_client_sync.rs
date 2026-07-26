//! 2대 동기화 시연: 실 서버 + 클라이언트 A/B 가 초대→조인→GK 전달→양방향 클립보드 동기화.
//!
//! 한 머신에서 두 독립 신원이 각자 in-memory 클립보드를 갖고, 서버(blind relay)를 거쳐
//! E2E 로 동기화되는 전 과정을 자동 시연한다. 실행: `cargo run -p sb-server --example two_client_sync`

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use sb_core::{EngineConfig, LocalOutcome, RemoteDecision, SyncEngine};
use sb_crypto::hash::sha256;
use sb_crypto::{
    build_add_from_blob, build_signed_rotation, invite, make_invite, open_rotation, seal_rotation,
    verify_chain, verify_rotation, wslog, GroupKey, Identity,
};
use sb_net::init_crypto;
use sb_proto::{C2s, ContentKind, EpochReason, KeyUpdate, LogEntry, Platform, Profile, S2c};
use sb_server::{serve, tls, Shared};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const PROFILE_AAD: &[u8] = b"sb/profile-v1";

fn profile_name(gk: &GroupKey, enc: &[u8]) -> Option<String> {
    let plain = gk.open_body(PROFILE_AAD, enc).ok()?;
    let p: Profile = sb_proto::decode(&plain).ok()?;
    Some(p.name)
}

fn cfg() -> EngineConfig {
    EngineConfig {
        enabled: true,
        sync_text: true,
        sync_images: true,
        max_content_bytes: 10 * 1024 * 1024,
        history_cap: 30,
    }
}

/// inbox 에서 원하는 변형이 나올 때까지 읽는다(presence 등은 건너뜀). 각 수신 3s 타임아웃.
macro_rules! recv_until {
    ($h:expr, $pat:pat => $body:expr) => {{
        let mut result = None;
        for _ in 0..500 {
            match tokio::time::timeout(std::time::Duration::from_secs(3), $h.inbox.recv()).await {
                Ok(Some($pat)) => {
                    result = Some($body);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => break, // 타임아웃 → 포기(어느 단계에서 막혔는지 expect 로 드러남)
            }
        }
        result
    }};
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_crypto();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  shareboard 2대 동기화 시연 (실 서버 + E2E 그룹 키)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // ── 서버 기동 ──
    let server_id = Identity::generate();
    let server_fp = server_id.device_id(); // = SHA-256(SPKI)
    let token = "demo-setup-token";
    let shared = Shared::new(Some(sha256(token.as_bytes())));
    let acceptor = TlsAcceptor::from(tls::server_config(&server_id).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(serve(listener, acceptor, shared));
    println!("[서버] 기동 @ {addr}");
    println!("[서버] 지문(SHA-256 SPKI): {}\n", hex8(&server_fp));

    // ── 클라이언트 A (창립자) ──
    let a = Identity::generate();
    println!("[A] 신원 생성: {}", hex8(&a.device_id()));
    let (genesis, wid) = wslog::build_genesis(&a, "디자인팀", now_ms());
    let mut a_log = vec![wslog::entry_bytes(&genesis)];
    let gk = GroupKey::generate(0); // A 가 그룹 키 생성

    let conn_a = sb_net::connect(addr, server_fp, &a).await?;
    let mut ha = sb_net::spawn(conn_a);
    ha.out
        .send(C2s::ClaimWorkspace {
            token: token.into(),
            genesis: a_log[0].clone(),
        })
        .await?;
    recv_until!(ha, S2c::AppendAck { .. } => ()).expect("claim ack");
    ha.out.send(C2s::Hello(hello(&a, 0, (0, [0u8; 32])))).await?;
    recv_until!(ha, S2c::Welcome(_) => ()).expect("welcome");
    println!("[A] 워크스페이스 '디자인팀' 생성 + 그룹 키 GK_0 보유\n");
    let mut a_engine = SyncEngine::new(a.device_id(), gk.clone(), cfg());

    // ── A 가 초대 코드 발급 ──
    let head = wslog::entry_hash(&a_log[0]);
    let (code, locator, blob) = make_invite(&a, wid, head, server_fp, now_ms() + 3_600_000)?;
    ha.out
        .send(C2s::PutInvite {
            locator,
            blob,
            ttl_s: 3600,
        })
        .await?;
    let code_display = invite::format_display(&code);
    println!("[A] 초대 코드 발급: ┃ {code_display} ┃  (B 에게 전달)\n");

    // ── 클라이언트 B (조인자) ──
    let b = Identity::generate();
    println!("[B] 신원 생성: {}", hex8(&b.device_id()));
    let conn_b = sb_net::connect(addr, server_fp, &b).await?;
    let mut hb = sb_net::spawn(conn_b);

    // B: 로그 취득 → 초대 blob → Add
    hb.out.send(C2s::GetLog { from_seq: 0 }).await?;
    let entries = recv_until!(hb, S2c::LogEntries { entries, .. } => entries).expect("log");
    let v = verify_chain(&entries, 0)?;
    let (loc_b, _k) = invite::derive(&code, &v.workspace_id)?;
    // A 의 PutInvite 가 서버에 반영될 때까지 재시도(두 연결이 동시 처리되는 레이스 흡수).
    let mut blob_b = None;
    for _ in 0..20 {
        hb.out.send(C2s::GetInviteBlob { locator: loc_b }).await?;
        if let Some(Some(b)) = recv_until!(hb, S2c::InviteBlob { blob } => blob) {
            blob_b = Some(b);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    let blob_b = blob_b.expect("invite blob (A PutInvite 반영 대기)");
    let (add, _secret) = build_add_from_blob(
        &code,
        v.workspace_id,
        &blob_b,
        &b.public(),
        v.head_hash,
        v.head_seq + 1,
        now_ms(),
    )?;
    let add_bytes = wslog::entry_bytes(&add);
    hb.out
        .send(C2s::AppendEntry {
            entry: add_bytes.clone(),
        })
        .await?;
    recv_until!(hb, S2c::AppendAck { .. } => ()).expect("add ack");
    println!("[B] 초대 코드로 조인 → 멤버 로그에 등록됨 (아직 GK 없음)\n");

    // ── A 가 B 의 Add 를 보고 GK 를 wrap 해 전달 ──
    let add_seen = recv_until!(ha, S2c::LogAppended { entry, .. } => entry).expect("A sees Add");
    a_log.push(add_seen.clone());
    let av = verify_chain(&a_log, 0)?;
    if let Ok(LogEntry::Add {
        subject_spki,
        subject_kem_pk,
        ..
    }) = sb_proto::decode::<LogEntry>(&add_seen)
    {
        let b_dev = sha256(&subject_spki);
        // 서명된 RotationBlob 을 B 의 KEM 공개키로 봉인 (§4.4).
        let rblob = build_signed_rotation(
            &a,
            gk.epoch(),
            *gk.expose(),
            EpochReason::Join,
            av.head_hash,
            av.member_set_hash(),
        );
        let wrapped = seal_rotation(&subject_kem_pk, &wid, &b_dev, &rblob)?;
        ha.out
            .send(C2s::PutKeyUpdate {
                updates: vec![KeyUpdate {
                    to: b_dev,
                    epoch: gk.epoch(),
                    wrap: wrapped,
                }],
            })
            .await?;
        println!("[A] B 의 공개키로 그룹 키를 봉인(wrap)해 전달 (서버는 암호문만 중계)\n");
    }

    // ── B: Hello → GK 수신·검증 ──
    hb.out.send(C2s::Hello(hello(&b, 0, (0, [0u8; 32])))).await?;
    let mut b_log = entries.clone();
    b_log.push(add_bytes);
    let bv = verify_chain(&b_log, 0)?;
    // Welcome.pending_key_update 또는 KeyUpdatePush 로 wrap 수신.
    let wrapped_for_b = recv_until!(hb,
        m @ (S2c::Welcome(_) | S2c::KeyUpdatePush { .. }) => match m {
            S2c::Welcome(w) => w.pending_key_update,
            S2c::KeyUpdatePush { wrap } => Some(wrap),
            _ => None,
        }
    )
    .flatten();
    let wrapped_for_b = match wrapped_for_b {
        Some(w) => w,
        None => recv_until!(hb, S2c::KeyUpdatePush { wrap } => wrap).expect("key update"),
    };
    let rblob = open_rotation(&b, &wid, &wrapped_for_b)?;
    assert!(
        verify_rotation(&rblob, &bv),
        "GK wrap 검증(서명·roster·epoch·member_set_hash)"
    );
    let gk_b = GroupKey::from_bytes(rblob.new_epoch, rblob.group_key);
    let mut b_engine = SyncEngine::new(b.device_id(), gk_b.clone(), cfg());
    println!("[B] 그룹 키 수신·검증 완료 → 이제 동기화 준비됨\n");

    // ── 기기 이름(프로필) E2E 교환 ──
    let a_profile = Profile {
        name: "엔지니어 A".into(),
        platform: Platform::Macos,
        log_head_hash: av.head_hash,
        seq: now_ms(),
        epoch: 0,
        ts_ms: now_ms(),
    };
    let b_profile = Profile {
        name: "디자이너 B".into(),
        platform: Platform::Macos,
        log_head_hash: bv.head_hash,
        seq: now_ms(),
        epoch: 0,
        ts_ms: now_ms(),
    };
    let a_enc = gk.seal_body(PROFILE_AAD, &sb_proto::encode(&a_profile)?)?;
    let b_enc = gk_b.seal_body(PROFILE_AAD, &sb_proto::encode(&b_profile)?)?;
    ha.out.send(C2s::SetProfile { epoch: 0, e2e: a_enc }).await?;
    hb.out.send(C2s::SetProfile { epoch: 0, e2e: b_enc }).await?;

    // 서버가 상대 presence(enc_profile + IP)를 중계 → 복호해 이름 확인.
    let (a_seen_addr, a_seen_prof) =
        recv_until!(hb, S2c::Presence { addr, enc_profile: Some(p), .. } => (addr, p))
            .expect("B sees A presence");
    println!(
        "[B] 멤버 목록: {} @ {}  (서버는 암호문만 중계, 이름 복호는 B가)",
        profile_name(&gk_b, &a_seen_prof).unwrap_or("?".into()),
        a_seen_addr.unwrap_or("?".into())
    );
    let (b_seen_addr, b_seen_prof) =
        recv_until!(ha, S2c::Presence { addr, enc_profile: Some(p), .. } => (addr, p))
            .expect("A sees B presence");
    println!(
        "[A] 멤버 목록: {} @ {}",
        profile_name(&gk, &b_seen_prof).unwrap_or("?".into()),
        b_seen_addr.unwrap_or("?".into())
    );
    println!();

    println!("─────────────  클립보드 동기화 시작  ─────────────\n");

    // ── A 가 복사 → B 가 수신 ──
    let msg1 = "안녕하세요, A 가 복사한 텍스트입니다 📋";
    println!("[A] 복사: \"{msg1}\"");
    let sig = match a_engine.on_local_clipboard(ContentKind::Text, msg1.as_bytes(), now_ms())? {
        LocalOutcome::Emit(s) => *s,
        o => panic!("Emit 기대: {o:?}"),
    };
    ha.out
        .send(C2s::ClipSignal {
            hdr: sig.hdr.clone(),
            e2e: sig.e2e.clone(),
        })
        .await?;
    let (origin, hdr, e2e) =
        recv_until!(hb, S2c::SignalFanout { origin, hdr, e2e } => (origin, hdr, e2e)).expect("B fanout");
    match b_engine.on_remote_signal(origin, hdr, &e2e, now_ms()) {
        RemoteDecision::ApplyInline { plaintext, .. } => {
            println!("[B] 수신·복호: \"{}\"", String::from_utf8_lossy(&plaintext));
            assert_eq!(plaintext, msg1.as_bytes());
            println!("    ✅ A → B 동기화 성공\n");
        }
        o => panic!("ApplyInline 기대: {o:?}"),
    }

    // ── B 가 복사 → A 가 수신 ──
    let msg2 = "이번엔 B 가 복사했어요 👋 reply from B";
    println!("[B] 복사: \"{msg2}\"");
    let sig2 = match b_engine.on_local_clipboard(ContentKind::Text, msg2.as_bytes(), now_ms())? {
        LocalOutcome::Emit(s) => *s,
        o => panic!("Emit 기대: {o:?}"),
    };
    hb.out
        .send(C2s::ClipSignal {
            hdr: sig2.hdr.clone(),
            e2e: sig2.e2e.clone(),
        })
        .await?;
    let (origin2, hdr2, e2e2) =
        recv_until!(ha, S2c::SignalFanout { origin, hdr, e2e } => (origin, hdr, e2e)).expect("A fanout");
    match a_engine.on_remote_signal(origin2, hdr2, &e2e2, now_ms()) {
        RemoteDecision::ApplyInline { plaintext, .. } => {
            println!("[A] 수신·복호: \"{}\"", String::from_utf8_lossy(&plaintext));
            assert_eq!(plaintext, msg2.as_bytes());
            println!("    ✅ B → A 동기화 성공\n");
        }
        o => panic!("ApplyInline 기대: {o:?}"),
    }

    // ── 보안 확인: 서버가 평문을 못 본다 ──
    println!("─────────────  보안 확인  ─────────────");
    println!("서버가 중계한 것은 GK 로 봉인된 암호문뿐 — 평문/그룹키/코드 미보유 (blind relay).");
    println!(
        "wire 상 signal e2e 예시(앞 24B nonce+ct): {}…",
        hex_n(&sig.e2e, 16)
    );
    println!("\n━━━━━━━━━━━━━━━━━━━━  시연 완료  ━━━━━━━━━━━━━━━━━━━━");
    Ok(())
}

fn hello(id: &Identity, epoch: u64, log_head: (u64, [u8; 32])) -> sb_proto::Hello {
    sb_proto::Hello {
        device_id: id.device_id(),
        proto_min: 2,
        proto_max: 2,
        app_version: "demo".into(),
        epoch,
        log_head,
    }
}

fn hex8(b: &[u8; 32]) -> String {
    hex_n(b, 8)
}
fn hex_n(b: &[u8], n: usize) -> String {
    let mut s = String::new();
    for x in b.iter().take(n) {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
