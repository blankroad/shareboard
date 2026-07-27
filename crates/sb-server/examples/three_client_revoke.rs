//! 멤버 강퇴 라이브 시연: 실 서버 + A(창립자)·B·C·D 4멤버.
//!
//! A 가 C 를 강퇴(Remove + Epoch 회전)하고 남은 멤버에게 새 그룹 키 GK_1 를 재-wrap 배달한다.
//! 검증 항목:
//!  1) B(온라인 잔류): LogAppended(Remove)→LogAppended(Epoch)→KeyUpdatePush 를 순서대로 받아
//!     GK_1 채택 → 강퇴 후에도 A 와 계속 동기화.
//!  2) C(강퇴 대상): roster 에서 빠져 릴레이가 fanout 하지 않고, 설령 신호를 받아도 GK_1 부재로
//!     복호 불가(EpochMismatch) — 접근 상실.
//!  3) D(오프라인 잔류): 강퇴 시점에 오프라인이라 wrap 이 서버 mailbox 에 적재 → 재접속 Welcome 으로
//!     log_tail + pending_key_update 수신. **로그를 먼저 반영한 뒤 wrap 을 검증**해야 채택된다는
//!     점(BUG 1 수정)을 잘못된 순서 vs 올바른 순서로 대비해 보여준다.
//!
//! 실행: `cargo run -p sb-server --example three_client_revoke`

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use sb_core::{EngineConfig, LocalOutcome, RemoteDecision, SyncEngine};
use sb_crypto::hash::sha256;
use sb_crypto::{
    build_add_from_blob, build_signed_rotation, invite, make_invite, open_rotation, seal_rotation,
    verify_chain, verify_rotation, wslog, GroupKey, Identity,
};
use sb_net::{init_crypto, ClientHandle};
use sb_proto::{C2s, ContentKind, EpochReason, KeyUpdate, LogEntry, S2c};
use sb_server::{serve, tls, Shared};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            match tokio::time::timeout(Duration::from_secs(3), $h.inbox.recv()).await {
                Ok(Some($pat)) => {
                    result = Some($body);
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        result
    }};
}

/// 새 멤버가 초대 코드로 조인: GetLog → InviteBlob(재시도) → Add append.
/// 반환 (핸들, add 바이트, 조인 시점 로그).
async fn guest_join(
    addr: SocketAddr,
    server_fp: [u8; 32],
    joiner: &Identity,
    code: &str,
) -> anyhow::Result<(ClientHandle, Vec<u8>, Vec<Vec<u8>>)> {
    let conn = sb_net::connect(addr, server_fp, joiner).await?;
    let mut h = sb_net::spawn(conn);
    h.out.send(C2s::GetLog { from_seq: 0 }).await?;
    let entries = recv_until!(h, S2c::LogEntries { entries, .. } => entries).expect("log");
    let v = verify_chain(&entries, 0)?;
    let (loc, _k) = invite::derive(code, &v.workspace_id)?;
    let mut blob = None;
    for _ in 0..20 {
        h.out.send(C2s::GetInviteBlob { locator: loc }).await?;
        if let Some(Some(b)) = recv_until!(h, S2c::InviteBlob { blob } => blob) {
            blob = Some(b);
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let blob = blob.expect("invite blob (PutInvite 반영 대기)");
    let (add, _s) = build_add_from_blob(
        code,
        v.workspace_id,
        &blob,
        &joiner.public(),
        v.head_hash,
        v.head_seq + 1,
        now_ms(),
    )?;
    let add_bytes = wslog::entry_bytes(&add);
    h.out
        .send(C2s::AppendEntry {
            entry: add_bytes.clone(),
        })
        .await?;
    recv_until!(h, S2c::AppendAck { .. } => ()).expect("add ack");
    let mut jlog = entries;
    jlog.push(add_bytes.clone());
    Ok((h, add_bytes, jlog))
}

/// A 가 새 1회용 초대를 발급(코드마다 grant 키 1개 → 조인 1건). 반환: 정규 코드.
async fn issue_invite(
    a: &Identity,
    ha: &mut ClientHandle,
    wid: [u8; 32],
    a_log: &[Vec<u8>],
    server_fp: [u8; 32],
) -> anyhow::Result<String> {
    let head = wslog::entry_hash(a_log.last().unwrap());
    let (code, locator, blob) = make_invite(a, wid, head, server_fp, now_ms() + 3_600_000)?;
    ha.out
        .send(C2s::PutInvite {
            locator,
            blob,
            ttl_s: 3600,
        })
        .await?;
    Ok(code.to_string())
}

/// A 가 조인자에게 GK 를 wrap 해 전달(§4.4, reason=Join). a_log 에 add 반영.
async fn deliver_gk(
    a: &Identity,
    ha: &mut ClientHandle,
    gk: &GroupKey,
    add_bytes: &[u8],
    a_log: &mut Vec<Vec<u8>>,
    wid: [u8; 32],
) -> anyhow::Result<()> {
    a_log.push(add_bytes.to_vec());
    let av = verify_chain(a_log, 0)?;
    if let Ok(LogEntry::Add {
        subject_spki,
        subject_kem_pk,
        ..
    }) = sb_proto::decode::<LogEntry>(add_bytes)
    {
        let dev = sha256(&subject_spki);
        let rblob = build_signed_rotation(
            a,
            gk.epoch(),
            *gk.expose(),
            EpochReason::Join,
            av.head_hash,
            av.member_set_hash(),
        );
        let wrapped = seal_rotation(&subject_kem_pk, &wid, &dev, &rblob)?;
        ha.out
            .send(C2s::PutKeyUpdate {
                updates: vec![KeyUpdate {
                    to: dev,
                    epoch: gk.epoch(),
                    wrap: wrapped,
                }],
            })
            .await?;
    }
    Ok(())
}

/// 조인자가 Hello 후 wrap 을 받아 검증·채택 → GK 반환.
async fn adopt_gk(
    h: &mut ClientHandle,
    joiner: &Identity,
    wid: [u8; 32],
    jlog: &[Vec<u8>],
) -> anyhow::Result<GroupKey> {
    h.out.send(C2s::Hello(hello(joiner, 0, (0, [0u8; 32])))).await?;
    let jv = verify_chain(jlog, 0)?;
    let wrapped = recv_until!(h,
        m @ (S2c::Welcome(_) | S2c::KeyUpdatePush { .. }) => match m {
            S2c::Welcome(w) => w.pending_key_update,
            S2c::KeyUpdatePush { wrap } => Some(wrap),
            _ => None,
        }
    )
    .flatten();
    let wrapped = match wrapped {
        Some(w) => w,
        None => recv_until!(h, S2c::KeyUpdatePush { wrap } => wrap).expect("key update"),
    };
    let rblob = open_rotation(joiner, &wid, &wrapped)?;
    anyhow::ensure!(verify_rotation(&rblob, &jv), "GK wrap 검증 실패");
    Ok(GroupKey::from_bytes(rblob.new_epoch, rblob.group_key))
}

/// A 가 텍스트를 복사해 ClipSignal 발행. (hdr, e2e) 반환(수신측 검증용).
async fn a_copy(
    a_engine: &mut SyncEngine,
    ha: &mut ClientHandle,
    text: &str,
) -> anyhow::Result<(sb_proto::SignalHdr, Vec<u8>)> {
    let sig = match a_engine.on_local_clipboard(ContentKind::Text, text.as_bytes(), now_ms())? {
        LocalOutcome::Emit(s) => *s,
        o => anyhow::bail!("Emit 기대: {o:?}"),
    };
    ha.out
        .send(C2s::ClipSignal {
            hdr: sig.hdr.clone(),
            e2e: sig.e2e.clone(),
        })
        .await?;
    Ok((sig.hdr, sig.e2e))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_crypto();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  shareboard 멤버 강퇴 시연 (실 서버 + GK 회전 + 재-wrap)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // ── 서버 기동 ──
    let server_id = Identity::generate();
    let server_fp = server_id.device_id();
    let token = "demo-setup-token";
    let shared = Shared::new(Some(sha256(token.as_bytes())));
    let acceptor = TlsAcceptor::from(tls::server_config(&server_id).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(serve(listener, acceptor, shared));
    println!("[서버] 기동 @ {addr}  (blind relay — 암호문만 중계)\n");

    // ── A (창립자) ──
    let a = Identity::generate();
    let (genesis, wid) = wslog::build_genesis(&a, "디자인팀", now_ms());
    let mut a_log = vec![wslog::entry_bytes(&genesis)];
    let gk0 = GroupKey::generate(0);
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
    let mut a_engine = SyncEngine::new(a.device_id(), gk0.clone(), cfg());
    println!(
        "[A] 워크스페이스 '디자인팀' 생성, 그룹 키 GK_0 보유  ({})",
        short(&a.device_id())
    );

    println!("[A] (조인자마다 별도 1회용 초대 발급)\n");

    // ── C (강퇴될 멤버) 조인 + GK_0 ──
    let c = Identity::generate();
    let code_c = issue_invite(&a, &mut ha, wid, &a_log, server_fp).await?;
    let (mut hc, add_c, c_log) = guest_join(addr, server_fp, &c, &code_c).await?;
    deliver_gk(&a, &mut ha, &gk0, &add_c, &mut a_log, wid).await?;
    let gk_c = adopt_gk(&mut hc, &c, wid, &c_log).await?;
    let mut c_engine = SyncEngine::new(c.device_id(), gk_c.clone(), cfg());
    println!("[C] 조인 + GK_0 수신 → 정상 멤버  ({})", short(&c.device_id()));

    // ── D (오프라인이 될 잔류 멤버) 조인 후 즉시 접속 종료 ──
    let d = Identity::generate();
    let code_d = issue_invite(&a, &mut ha, wid, &a_log, server_fp).await?;
    let (hd, add_d, d_log) = guest_join(addr, server_fp, &d, &code_d).await?;
    a_log.push(add_d.clone()); // A 는 로그만 반영(D 는 곧 오프라인 → GK 는 강퇴 시 mailbox 로).
    let d_head_seq = verify_chain(&d_log, 0)?.head_seq;
    let d_head_hash = wslog::entry_hash(d_log.last().unwrap());
    drop(hd); // D 오프라인.
    println!("[D] 조인(로스터 등록) 후 오프라인  ({})", short(&d.device_id()));

    // ── B (온라인 잔류 멤버) 조인 + GK_0 ── (마지막에 조인 → 이후 stray LogAppended 없음)
    let b = Identity::generate();
    let code_b = issue_invite(&a, &mut ha, wid, &a_log, server_fp).await?;
    let (mut hb, add_b, mut b_log) = guest_join(addr, server_fp, &b, &code_b).await?;
    deliver_gk(&a, &mut ha, &gk0, &add_b, &mut a_log, wid).await?;
    let gk_b = adopt_gk(&mut hb, &b, wid, &b_log).await?;
    let mut b_engine = SyncEngine::new(b.device_id(), gk_b.clone(), cfg());
    println!("[B] 조인 + GK_0 수신 → 정상 멤버  ({})\n", short(&b.device_id()));

    let roster0 = verify_chain(&a_log, 0)?;
    println!(
        "현재 로스터: {}명 (A, B, C + 오프라인 D), epoch {}\n",
        roster0.members.len(),
        roster0.epoch
    );

    // ── 강퇴 전 정상 동기화 확인(GK_0): A → B, C ──
    println!("─────────────  강퇴 전: 정상 동기화 (GK_0)  ─────────────");
    let (hdr0, e2e0) = a_copy(&mut a_engine, &mut ha, "강퇴 전 메시지 (GK_0)").await?;
    let _ = (hdr0, e2e0);
    for (name, h, eng) in [("B", &mut hb, &mut b_engine), ("C", &mut hc, &mut c_engine)] {
        let (origin, hdr, e2e) = recv_until!(h, S2c::SignalFanout { origin, hdr, e2e } => (origin, hdr, e2e))
            .unwrap_or_else(|| panic!("{name} fanout"));
        match eng.on_remote_signal(origin, hdr, &e2e, now_ms()) {
            RemoteDecision::ApplyInline { plaintext, .. } => {
                println!("[{name}] 복호 성공: \"{}\"", String::from_utf8_lossy(&plaintext));
            }
            o => panic!("[{name}] ApplyInline 기대: {o:?}"),
        }
    }
    println!();

    // ══════════════  강퇴: A 가 C 를 내보냄  ══════════════
    println!("═════════════  A 가 C 를 강퇴 (Remove + Epoch 회전)  ═════════════");
    let v = verify_chain(&a_log, 0)?;
    // 1) Remove(잔존 멤버 A 서명).
    let remove = wslog::build_remove(&a, v.head_hash, v.head_seq + 1, c.device_id(), now_ms());
    let remove_bytes = wslog::entry_bytes(&remove);
    ha.out
        .send(C2s::AppendEntry {
            entry: remove_bytes.clone(),
        })
        .await?;
    recv_until!(ha, S2c::AppendAck { .. } => ()).expect("remove ack");
    a_log.push(remove_bytes.clone());

    // 2) 제거 후 roster → member_set_hash + 새 GK.
    let v2 = verify_chain(&a_log, 0)?;
    anyhow::ensure!(!v2.is_member(&c.device_id()), "C 제거됨");
    let msh = v2.member_set_hash();
    let new_epoch = v.epoch + 1;
    let new_gk = GroupKey::generate(new_epoch);

    // 3) Epoch(회전자 A 서명, reason=Revoke).
    let epoch_e = wslog::build_epoch(
        &a,
        v2.head_hash,
        v2.head_seq + 1,
        new_epoch,
        EpochReason::Revoke(c.device_id()),
        msh,
        [0u8; 32],
        now_ms(),
    );
    let epoch_bytes = wslog::entry_bytes(&epoch_e);
    let epoch_hash = wslog::entry_hash(&epoch_bytes);
    ha.out
        .send(C2s::AppendEntry {
            entry: epoch_bytes.clone(),
        })
        .await?;
    recv_until!(ha, S2c::AppendAck { .. } => ()).expect("epoch ack");
    a_log.push(epoch_bytes.clone());

    // 4) 남은 멤버(B 온라인, D 오프라인)에게 GK_1 재-wrap.
    let blob = build_signed_rotation(
        &a,
        new_epoch,
        *new_gk.expose(),
        EpochReason::Revoke(c.device_id()),
        epoch_hash,
        msh,
    );
    let a_id = a.device_id();
    let mut updates = Vec::new();
    for (did, mi) in &v2.members {
        if *did == a_id {
            continue;
        }
        let wrap = seal_rotation(&mi.kem_pk, &wid, did, &blob)?;
        updates.push(KeyUpdate {
            to: *did,
            epoch: new_epoch,
            wrap,
        });
    }
    println!(
        "[A] Remove+Epoch 로그 반영, GK_1 을 남은 멤버 {}명에게 재-wrap(B 즉시 배달, D 는 mailbox)",
        updates.len()
    );
    ha.out.send(C2s::PutKeyUpdate { updates }).await?;
    a_engine.set_group_key(new_gk.clone()); // A 자신도 GK_1 채택.
    println!("[A] epoch {} → {} 회전 완료\n", v.epoch, new_epoch);

    // ── B(온라인): Remove → Epoch → KeyUpdate 순서로 수신 → GK_1 채택 ──
    println!("─────────────  B(온라인 잔류): 새 GK_1 채택  ─────────────");
    let mut gk_b1 = None;
    for _ in 0..500 {
        match tokio::time::timeout(Duration::from_secs(3), hb.inbox.recv()).await {
            Ok(Some(S2c::LogAppended { entry, .. })) => {
                b_log.push(entry);
            }
            Ok(Some(S2c::KeyUpdatePush { wrap })) => {
                let bv = verify_chain(&b_log, 0)?;
                let rb = open_rotation(&b, &wid, &wrap)?;
                anyhow::ensure!(verify_rotation(&rb, &bv), "B 회전 검증");
                gk_b1 = Some(GroupKey::from_bytes(rb.new_epoch, rb.group_key));
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    let gk_b1 = gk_b1.expect("B 가 GK_1 채택");
    b_engine.set_group_key(gk_b1.clone());
    let bv = verify_chain(&b_log, 0)?;
    println!(
        "[B] LogAppended(Remove)·LogAppended(Epoch) 반영 → epoch {} → KeyUpdate 검증·채택 ✅",
        bv.epoch
    );
    anyhow::ensure!(!bv.is_member(&c.device_id()), "B 로스터에서도 C 제거");
    println!("    B 로스터: {}명 (C 빠짐)\n", bv.members.len());

    // ── D(오프라인 잔류) 재접속: Welcome 으로 log_tail + mailbox wrap 수신 ──
    println!("─────────────  D(오프라인 잔류): 재접속 후 GK_1 채택  ─────────────");
    let conn_d2 = sb_net::connect(addr, server_fp, &d).await?;
    let mut hd2 = sb_net::spawn(conn_d2);
    hd2.out
        .send(C2s::Hello(hello(&d, 0, (d_head_seq, d_head_hash))))
        .await?;
    let w = recv_until!(hd2, S2c::Welcome(w) => w).expect("D welcome");
    let d_wrap = w.pending_key_update.clone().expect("D mailbox wrap 수신");

    // BUG 1 대비: (a) 로그 반영 전 검증 = 실패, (b) 로그 반영 후 검증 = 성공.
    let stale = verify_chain(&d_log, 0)?; // 아직 강퇴 회전 미반영(epoch 0).
    let rb_d = open_rotation(&d, &wid, &d_wrap)?;
    let ok_before = verify_rotation(&rb_d, &stale);
    println!(
        "[D] ❌ (구 순서) 로그 반영 전 wrap 검증 → {}  (epoch·roster 불일치로 탈락)",
        if ok_before { "성공" } else { "실패" }
    );
    // 수정 순서: Welcome.log_tail 먼저 반영.
    let mut d_full = d_log.clone();
    d_full.extend(w.log_tail.clone());
    let dv = verify_chain(&d_full, 0)?;
    let ok_after = verify_rotation(&rb_d, &dv);
    println!(
        "[D] ✅ (수정 순서) log_tail 먼저 반영(epoch {}) 후 wrap 검증 → {}",
        dv.epoch,
        if ok_after { "성공" } else { "실패" }
    );
    anyhow::ensure!(
        !ok_before && ok_after,
        "BUG 1: 반드시 로그 먼저 반영해야 채택 가능"
    );
    let gk_d1 = GroupKey::from_bytes(rb_d.new_epoch, rb_d.group_key);
    let mut d_engine = SyncEngine::new(d.device_id(), gk_d1.clone(), cfg());
    println!("    D 로스터: {}명 (C 빠짐)\n", dv.members.len());

    // ── 강퇴 후 동기화(GK_1): A → B, D 성공 / C 차단 ──
    println!("─────────────  강퇴 후: 동기화 (GK_1)  ─────────────");
    let (hdr1, e2e1) = a_copy(&mut a_engine, &mut ha, "강퇴 후 메시지 (GK_1)").await?;
    for (name, h, eng) in [("B", &mut hb, &mut b_engine), ("D", &mut hd2, &mut d_engine)] {
        let (origin, hdr, e2e) = recv_until!(h, S2c::SignalFanout { origin, hdr, e2e } => (origin, hdr, e2e))
            .unwrap_or_else(|| panic!("{name} fanout"));
        match eng.on_remote_signal(origin, hdr, &e2e, now_ms()) {
            RemoteDecision::ApplyInline { plaintext, .. } => {
                println!(
                    "[{name}] 복호 성공: \"{}\"  ✅",
                    String::from_utf8_lossy(&plaintext)
                );
            }
            o => panic!("[{name}] ApplyInline 기대: {o:?}"),
        }
    }

    // C 차단 검증: (1) 릴레이가 fanout 하지 않음(수신 없음) (2) 신호를 강제로 줘도 복호 불가.
    let got = recv_until!(hc, S2c::SignalFanout { .. } => ());
    anyhow::ensure!(got.is_none(), "C 는 로스터 밖 → fanout 수신 없어야 함");
    println!("[C] 릴레이 fanout 수신: 없음 (로스터에서 제외됨)  ✅");
    match c_engine.on_remote_signal(a.device_id(), hdr1.clone(), &e2e1, now_ms()) {
        RemoteDecision::Ignore(reason) => {
            println!("[C] 강제로 신호를 줘도 복호 불가: Ignore({reason:?}) — GK_1 미보유  ✅");
        }
        o => panic!("[C] Ignore(EpochMismatch) 기대: {o:?}"),
    }

    println!("\n━━━━━━━━━━━━━━━━━━━━  강퇴 시연 완료  ━━━━━━━━━━━━━━━━━━━━");
    println!("요약: C 강퇴 → GK_1 회전 → B(온라인)·D(오프라인 재접속) 채택, C 접근 상실.");
    println!("      서버는 전 과정에서 암호문/서명본만 중계 — GK·평문 미보유(blind relay).");
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

fn short(b: &[u8; 32]) -> String {
    let mut s = String::from("id:");
    for x in b.iter().take(4) {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
