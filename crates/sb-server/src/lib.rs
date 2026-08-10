//! # sb-server
//!
//! shareboard blind relay 서버 (PLAN.md §4.5, §5). 서버는 암호문·서명본만 취급하며
//! 그룹 키·평문·초대 코드를 어떤 시점에도 보유하지 않는다.
//!
//! - 멤버십 = 워크스페이스 로그(sb-crypto `verify_chain`)로 판정 — 서버는 append 직렬화만.
//! - ClipSignal fanout(발신자 제외), 콘텐츠 pull 릴레이, head 캐시, presence, mailbox.

pub mod config;
pub mod tls;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;
use tokio_util::codec::Framed;

use sb_crypto::hash::sha256;
use sb_crypto::verify_chain;
use sb_net::{cert_fingerprint, framed_codec};
use sb_proto::params::HEAD_CACHE_DEPTH;
use sb_proto::{
    decode_env, encode_env, AppendRejectReason, ByeReason, C2s, ContentId, DeviceId, Epoch, ErrorCode,
    LogEntry, RejectReason, S2c, SignalHdr, Welcome,
};

type Tx = mpsc::UnboundedSender<S2c>;

/// 서버 공유 상태.
pub struct Shared {
    inner: Mutex<Inner>,
}

struct Inner {
    setup_token_hash: Option<[u8; 32]>,
    claimed: bool,
    log: Vec<Vec<u8>>,
    epoch: Epoch,
    head_cache: VecDeque<(DeviceId, SignalHdr, Vec<u8>)>,
    sessions: HashMap<DeviceId, Tx>,
    /// device_id → 접속 주소(멤버 식별용, presence 에 스탬프).
    addrs: HashMap<DeviceId, String>,
    profiles: HashMap<DeviceId, Vec<u8>>,
    invites: HashMap<[u8; 32], Vec<u8>>,
    mailbox: HashMap<DeviceId, Vec<u8>>,
    pending_pull: HashMap<ContentId, DeviceId>,
    /// 설정 시 워크스페이스 로그를 여기 영속화(호스트 앱 재시작에도 유지). None = 메모리 전용.
    data_dir: Option<std::path::PathBuf>,
}

/// 워크스페이스 로그 영속 파일 경로.
fn log_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("wslog.cbor")
}

/// 로그를 디스크에서 로드 → (log, claimed, epoch).
fn load_log(dir: &std::path::Path) -> (Vec<Vec<u8>>, bool, Epoch) {
    match std::fs::read(log_path(dir)) {
        Ok(bytes) => {
            let log: Vec<Vec<u8>> = sb_proto::decode(&bytes).unwrap_or_default();
            let epoch = verify_chain(&log, 0).map(|v| v.epoch).unwrap_or(0);
            let claimed = !log.is_empty();
            (log, claimed, epoch)
        }
        Err(_) => (Vec::new(), false, 0),
    }
}

/// 로그를 디스크에 저장(영속 활성 시).
fn save_log(inner: &Inner) {
    if let Some(dir) = &inner.data_dir {
        let _ = std::fs::create_dir_all(dir);
        if let Ok(bytes) = sb_proto::encode(&inner.log) {
            let _ = std::fs::write(log_path(dir), bytes);
        }
    }
}

impl Inner {
    /// 검증된 로그로부터 현 roster(멤버 device_id 집합).
    fn roster(&self) -> BTreeSet<DeviceId> {
        if self.log.is_empty() {
            return BTreeSet::new();
        }
        match verify_chain(&self.log, 0) {
            Ok(v) => v.members.keys().copied().collect(),
            Err(_) => BTreeSet::new(),
        }
    }
}

fn send_to(inner: &Inner, dev: &DeviceId, msg: S2c) {
    if let Some(tx) = inner.sessions.get(dev) {
        let _ = tx.send(msg);
    }
}

/// 멤버 전원(except 제외)에게 브로드캐스트.
fn broadcast_members(inner: &Inner, roster: &BTreeSet<DeviceId>, except: &DeviceId, msg: S2c) {
    for (d, t) in inner.sessions.iter() {
        if d != except && roster.contains(d) {
            let _ = t.send(msg.clone());
        }
    }
}

impl Shared {
    pub fn new(setup_token_hash: Option<[u8; 32]>) -> Arc<Self> {
        Self::build(setup_token_hash, Vec::new(), false, 0, None)
    }

    /// 영속 활성 — `data_dir/wslog.cbor` 에서 로그를 로드하고 이후 변경을 저장한다.
    pub fn with_persistence(setup_token_hash: Option<[u8; 32]>, data_dir: std::path::PathBuf) -> Arc<Self> {
        let (log, claimed, epoch) = load_log(&data_dir);
        Self::build(setup_token_hash, log, claimed, epoch, Some(data_dir))
    }

    /// 워크스페이스가 이미 생성(클레임)되었는가 — 기동 로그/진단용.
    pub async fn is_claimed(&self) -> bool {
        self.inner.lock().await.claimed
    }

    fn build(
        setup_token_hash: Option<[u8; 32]>,
        log: Vec<Vec<u8>>,
        claimed: bool,
        epoch: Epoch,
        data_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                setup_token_hash,
                claimed,
                log,
                epoch,
                head_cache: VecDeque::new(),
                sessions: HashMap::new(),
                addrs: HashMap::new(),
                profiles: HashMap::new(),
                invites: HashMap::new(),
                mailbox: HashMap::new(),
                pending_pull: HashMap::new(),
                data_dir,
            }),
        })
    }

    async fn on_connect(&self, dev: DeviceId, tx: Tx, addr: String) {
        let mut g = self.inner.lock().await;
        g.sessions.insert(dev, tx);
        g.addrs.insert(dev, addr.clone());
        let roster = g.roster();
        if roster.contains(&dev) {
            let profile = g.profiles.get(&dev).cloned();
            broadcast_members(
                &g,
                &roster,
                &dev,
                S2c::Presence {
                    device_id: dev,
                    online: true,
                    addr: Some(addr),
                    enc_profile: profile,
                },
            );
        }
    }

    async fn on_disconnect(&self, dev: DeviceId) {
        let mut g = self.inner.lock().await;
        g.sessions.remove(&dev);
        g.addrs.remove(&dev);
        let roster = g.roster();
        broadcast_members(
            &g,
            &roster,
            &dev,
            S2c::Presence {
                device_id: dev,
                online: false,
                addr: None,
                enc_profile: None,
            },
        );
    }

    /// 한 메시지 처리. `false` 반환 시 연결 종료.
    async fn handle(&self, dev: DeviceId, msg: C2s) -> bool {
        let mut g = self.inner.lock().await;
        let roster = g.roster();
        let is_member = roster.contains(&dev);

        // Guest lane 허용 메시지 화이트리스트 (§4.5).
        let guest_ok = matches!(
            msg,
            C2s::ClaimWorkspace { .. }
                | C2s::GetInviteBlob { .. }
                | C2s::GetLog { .. }
                | C2s::AppendEntry { .. }
        );
        if !is_member && !guest_ok {
            send_to(
                &g,
                &dev,
                S2c::Error {
                    code: ErrorCode::NotMember,
                    detail: "member 아님".into(),
                },
            );
            return true;
        }

        match msg {
            C2s::Hello(h) => {
                if h.proto_max < sb_proto::params::PROTO_MIN || h.proto_min > sb_proto::params::PROTO_MAX {
                    send_to(
                        &g,
                        &dev,
                        S2c::Error {
                            code: ErrorCode::VersionIncompatible,
                            detail: "version".into(),
                        },
                    );
                    return false;
                }
                let (from_seq, from_hash) = h.log_head;
                let tail: Vec<Vec<u8>> = if from_seq == 0 && from_hash == [0u8; 32] {
                    g.log.clone()
                } else {
                    g.log.iter().skip(from_seq as usize + 1).cloned().collect()
                };
                let presence = roster
                    .iter()
                    .map(|d| sb_proto::PresenceEntry {
                        device_id: *d,
                        online: g.sessions.contains_key(d),
                        addr: g.addrs.get(d).cloned(),
                        enc_profile: g.profiles.get(d).cloned(),
                    })
                    .collect();
                let head = g.head_cache.iter().cloned().collect();
                let pending = g.mailbox.remove(&dev);
                let w = Welcome {
                    chosen_version: sb_proto::params::PROTO_MAX,
                    epoch: g.epoch,
                    log_tail: tail,
                    pending_key_update: pending,
                    presence,
                    head,
                    server_time_ms: 0,
                };
                send_to(&g, &dev, S2c::Welcome(w));
            }

            C2s::ClipSignal { hdr, e2e } => {
                g.head_cache.push_back((dev, hdr.clone(), e2e.clone()));
                while g.head_cache.len() > HEAD_CACHE_DEPTH {
                    g.head_cache.pop_front();
                }
                broadcast_members(
                    &g,
                    &roster,
                    &dev,
                    S2c::SignalFanout {
                        origin: dev,
                        hdr,
                        e2e,
                    },
                );
            }

            C2s::ContentRequest { id, .. } => {
                if let Some((origin, _, _)) = g.head_cache.iter().find(|(_, h, _)| h.id == id).cloned() {
                    g.pending_pull.insert(id, dev);
                    send_to(&g, &origin, S2c::ContentPull { id });
                } else {
                    send_to(
                        &g,
                        &dev,
                        S2c::ContentReject {
                            id,
                            reason: RejectReason::Gone,
                        },
                    );
                }
            }
            C2s::ContentBegin {
                id,
                ct_size,
                chunk_count,
                chunk_size,
            } => {
                if let Some(req) = g.pending_pull.get(&id).copied() {
                    send_to(
                        &g,
                        &req,
                        S2c::ContentBegin {
                            id,
                            ct_size,
                            chunk_count,
                            chunk_size,
                        },
                    );
                }
            }
            C2s::ContentChunk { id, index, data } => {
                if let Some(req) = g.pending_pull.get(&id).copied() {
                    send_to(&g, &req, S2c::ContentChunk { id, index, data });
                }
            }
            C2s::ContentReject { id, reason } => {
                if let Some(req) = g.pending_pull.remove(&id) {
                    send_to(&g, &req, S2c::ContentReject { id, reason });
                }
            }
            C2s::ContentAbort { id, reason } => {
                if let Some(req) = g.pending_pull.remove(&id) {
                    send_to(&g, &req, S2c::ContentAbort { id, reason });
                }
            }

            C2s::ClaimWorkspace { token, genesis } => {
                if g.claimed {
                    send_to(
                        &g,
                        &dev,
                        S2c::Error {
                            code: ErrorCode::ProtocolError,
                            detail: "이미 클레임됨".into(),
                        },
                    );
                    return true;
                }
                if let Some(expected) = g.setup_token_hash {
                    if sha256(token.as_bytes()) != expected {
                        send_to(
                            &g,
                            &dev,
                            S2c::Error {
                                code: ErrorCode::NotMember,
                                detail: "토큰 불일치".into(),
                            },
                        );
                        return false;
                    }
                }
                match verify_chain(&[genesis.clone()], 0) {
                    Ok(v) => {
                        g.log = vec![genesis];
                        g.claimed = true;
                        g.epoch = 0;
                        save_log(&g);
                        send_to(
                            &g,
                            &dev,
                            S2c::AppendAck {
                                seq: 0,
                                head_hash: v.head_hash,
                            },
                        );
                    }
                    Err(_) => {
                        send_to(
                            &g,
                            &dev,
                            S2c::Error {
                                code: ErrorCode::ProtocolError,
                                detail: "genesis 무효".into(),
                            },
                        );
                        return false;
                    }
                }
            }

            C2s::GetLog { from_seq } => {
                let entries: Vec<Vec<u8>> = g.log.iter().skip(from_seq as usize).cloned().collect();
                send_to(&g, &dev, S2c::LogEntries { entries, done: true });
            }

            C2s::AppendEntry { entry } => {
                // 멤버는 모든 엔트리, 게스트는 Add 만 허용(조인 §4.3.4). Add 의 정당성은 아래
                // verify_chain 이 보장(grant_cert 가 sponsor 멤버 서명 + grant_sk 서명) — 임의
                // 게스트의 위조 Add 는 체인 검증에서 탈락. locator 바인딩(§4.5)은 DoS 완화용 후속.
                let is_add = matches!(sb_proto::decode::<LogEntry>(&entry), Ok(LogEntry::Add { .. }));
                if !is_member && !is_add {
                    send_to(
                        &g,
                        &dev,
                        S2c::AppendReject {
                            reason: AppendRejectReason::NotAuthorized,
                        },
                    );
                    return true;
                }
                let mut trial = g.log.clone();
                trial.push(entry.clone());
                match verify_chain(&trial, 0) {
                    Ok(v) if v.accepted == trial.len() => {
                        g.log.push(entry.clone());
                        g.epoch = v.epoch;
                        save_log(&g);
                        if let Ok(LogEntry::Epoch { .. }) = sb_proto::decode::<LogEntry>(&entry) {
                            g.head_cache.clear();
                        }
                        let seq = v.head_seq;
                        let head_hash = v.head_hash;
                        send_to(&g, &dev, S2c::AppendAck { seq, head_hash });
                        let new_roster = g.roster();
                        broadcast_members(&g, &new_roster, &dev, S2c::LogAppended { entry, seq });
                    }
                    _ => {
                        send_to(
                            &g,
                            &dev,
                            S2c::AppendReject {
                                reason: AppendRejectReason::Conflict,
                            },
                        );
                    }
                }
            }

            C2s::PutInvite { locator, blob, .. } => {
                g.invites.insert(locator, blob);
            }
            C2s::RevokeInvite { locator } => {
                g.invites.remove(&locator);
            }
            C2s::GetInviteBlob { locator } => {
                let blob = g.invites.get(&locator).cloned();
                send_to(&g, &dev, S2c::InviteBlob { blob });
            }

            C2s::PutKeyUpdate { updates } => {
                for u in updates {
                    if g.sessions.contains_key(&u.to) {
                        send_to(&g, &u.to, S2c::KeyUpdatePush { wrap: u.wrap });
                    } else {
                        g.mailbox.insert(u.to, u.wrap);
                    }
                }
            }

            C2s::SetProfile { e2e, .. } => {
                g.profiles.insert(dev, e2e.clone());
                broadcast_members(
                    &g,
                    &roster,
                    &dev,
                    S2c::Presence {
                        device_id: dev,
                        online: true,
                        addr: g.addrs.get(&dev).cloned(),
                        enc_profile: Some(e2e),
                    },
                );
            }

            C2s::Ping { nonce } => {
                send_to(&g, &dev, S2c::Pong { nonce });
            }
            C2s::Leave => return false,
            C2s::Bye { .. } => return false,
        }
        true
    }
}

/// accept 루프. 각 연결을 별도 task 로 처리.
pub async fn serve(listener: TcpListener, acceptor: TlsAcceptor, shared: Arc<Shared>) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("accept 실패: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(tcp, peer.to_string(), acceptor, shared).await {
                tracing::debug!("연결 종료: {e}");
            }
        });
    }
}

async fn handle_conn(
    tcp: TcpStream,
    peer: String,
    acceptor: TlsAcceptor,
    shared: Arc<Shared>,
) -> anyhow::Result<()> {
    tcp.set_nodelay(true).ok();
    let tls = acceptor.accept(tcp).await?;
    let dev = {
        let (_io, conn) = tls.get_ref();
        let certs = conn
            .peer_certificates()
            .ok_or_else(|| anyhow::anyhow!("클라 cert 없음"))?;
        let first = certs.first().ok_or_else(|| anyhow::anyhow!("빈 cert 체인"))?;
        cert_fingerprint(first)
    };

    let framed = Framed::new(tls, framed_codec());
    let (mut sink, mut stream) = framed.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<S2c>();

    shared.on_connect(dev, tx, peer).await;

    let mut writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(bytes) = encode_env(msg) {
                let frame: bytes::Bytes = bytes.into();
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(item) = stream.next().await {
        let frame = match item {
            Ok(f) => f,
            Err(_) => break,
        };
        let msg: C2s = match decode_env(&frame) {
            Ok(m) => m,
            Err(_) => break,
        };
        if !shared.handle(dev, msg).await {
            let _ = shared; // keep alive
            break;
        }
    }

    // on_disconnect 가 세션 맵에서 tx 를 지우면 writer 의 rx 가 닫혀 루프가 자연 종료된다.
    shared.on_disconnect(dev).await;
    // 곧바로 abort 하면 큐에 남은 프레임이 버려진다 — 거부 사유(Error: 토큰 불일치 등)를
    // 보내고 세션을 끊는 경로에서 클라가 사유 대신 EOF 만 보게 되므로, flush 를 기다린다.
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    let _ = ByeReason::Shutdown; // 사용 표식
    Ok(())
}

#[cfg(test)]
mod integration {
    //! 통합 테스트: 1 서버 + 2 클라이언트가 릴레이를 거쳐 E2E 동기화.
    use super::*;
    use std::net::SocketAddr;

    use rustls::pki_types::CertificateDer;
    use tokio::net::TcpListener;

    use sb_core::{EngineConfig, LocalOutcome, RemoteDecision, SyncEngine};
    use sb_crypto::{wslog, GroupKey, Identity};
    use sb_net::{cert_fingerprint, init_crypto};
    use sb_proto::{ContentKind, Hello};

    fn cfg() -> EngineConfig {
        EngineConfig {
            enabled: true,
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_content_bytes: 10 * 1024 * 1024,
            history_cap: 30,
        }
    }

    /// presence 등 잡음을 건너뛰고 다음 SignalFanout 을 받는다.
    async fn recv_fanout(conn: &mut sb_net::Connection) -> (DeviceId, SignalHdr, Vec<u8>) {
        loop {
            match conn.recv().await.unwrap() {
                S2c::SignalFanout { origin, hdr, e2e } => return (origin, hdr, e2e),
                S2c::Presence { .. } | S2c::LogAppended { .. } => continue,
                other => panic!("SignalFanout 기대, got {other:?}"),
            }
        }
    }

    async fn start_server(shared: Arc<Shared>, server_id: &Identity) -> SocketAddr {
        let acceptor = TlsAcceptor::from(tls::server_config(server_id).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, acceptor, shared));
        addr
    }

    #[tokio::test]
    async fn two_clients_e2e_sync_through_relay() {
        init_crypto();

        let server_id = Identity::generate();
        let (scert_der, _) = server_id.tls_material().unwrap();
        let server_fp = cert_fingerprint(&CertificateDer::from(scert_der));
        let shared = Shared::new(Some(sha256(b"setup-token")));
        let addr = start_server(shared.clone(), &server_id).await;

        // 창립자 A, 조인자 B.
        let a = Identity::generate();
        let b = Identity::generate();

        // A 접속 → 워크스페이스 클레임.
        let (genesis, wid) = wslog::build_genesis(&a, "eng-team", 1);
        let mut ca = sb_net::connect(addr, server_fp, &a).await.unwrap();
        ca.send(C2s::ClaimWorkspace {
            token: "setup-token".into(),
            genesis: wslog::entry_bytes(&genesis),
        })
        .await
        .unwrap();
        assert!(matches!(ca.recv().await.unwrap(), S2c::AppendAck { seq: 0, .. }));

        // A 가 B 를 초대(grant) → Add 엔트리 append.
        let grant = sb_crypto::generate_grant();
        let gc = wslog::build_grant_cert(&a, grant.pk_der.clone(), 10_000, wid);
        let head = wslog::entry_hash(&wslog::entry_bytes(&genesis));
        let add = wslog::build_add(&grant.sk, gc, &b.public(), head, 1, 2);
        ca.send(C2s::AppendEntry {
            entry: wslog::entry_bytes(&add),
        })
        .await
        .unwrap();
        assert!(matches!(ca.recv().await.unwrap(), S2c::AppendAck { seq: 1, .. }));

        // 두 엔진에 동일 GK 주입(실제 배포는 wrap — sb-crypto 에서 별도 검증).
        let gk = GroupKey::from_bytes(0, [77u8; 32]);
        let mut ea = SyncEngine::new(a.device_id(), gk.clone(), cfg());
        let mut eb = SyncEngine::new(b.device_id(), gk, cfg());

        // B 접속 → Hello/Welcome(로그 tail 수신).
        let mut cb = sb_net::connect(addr, server_fp, &b).await.unwrap();
        cb.send(C2s::Hello(Hello {
            device_id: b.device_id(),
            proto_min: 2,
            proto_max: 2,
            app_version: "test".into(),
            epoch: 0,
            log_head: (0, [0u8; 32]),
        }))
        .await
        .unwrap();
        let welcome = cb.recv().await.unwrap();
        match welcome {
            S2c::Welcome(w) => {
                // 로그 전체(genesis+Add) 를 tail 로 받아 체인 검증 → B 도 멤버 확인.
                assert_eq!(w.log_tail.len(), 2);
                let v = verify_chain(&w.log_tail, 0).unwrap();
                assert!(v.is_member(&a.device_id()) && v.is_member(&b.device_id()));
            }
            other => panic!("Welcome 기대: {other:?}"),
        }

        // A 가 클립보드 복사 → 신호 발행 → 서버 fanout → B 적용.
        let sig = match ea
            .on_local_clipboard(ContentKind::Text, b"hello team", 1000)
            .unwrap()
        {
            LocalOutcome::Emit(s) => *s,
            o => panic!("Emit 기대: {o:?}"),
        };
        ca.send(C2s::ClipSignal {
            hdr: sig.hdr.clone(),
            e2e: sig.e2e.clone(),
        })
        .await
        .unwrap();

        let (origin, hdr, e2e) = recv_fanout(&mut cb).await;
        assert_eq!(origin, a.device_id(), "서버가 origin 스탬프");
        let dec = eb.on_remote_signal(origin, hdr, &e2e, 1001);
        assert_eq!(
            dec,
            RemoteDecision::ApplyInline {
                id: sig.hdr.id,
                kind: ContentKind::Text,
                plaintext: b"hello team".to_vec()
            },
            "B 가 A 의 클립보드를 E2E 복호해 적용"
        );

        // 역방향: B 복사 → A 수신.
        let sig2 = match eb
            .on_local_clipboard(ContentKind::Text, b"reply from B", 2000)
            .unwrap()
        {
            LocalOutcome::Emit(s) => *s,
            o => panic!("Emit 기대: {o:?}"),
        };
        cb.send(C2s::ClipSignal {
            hdr: sig2.hdr.clone(),
            e2e: sig2.e2e.clone(),
        })
        .await
        .unwrap();
        // A 는 Hello 를 보내지 않았지만 세션·멤버이므로 fanout 대상.
        let (origin, hdr, e2e) = recv_fanout(&mut ca).await;
        assert_eq!(origin, b.device_id());
        let dec = ea.on_remote_signal(origin, hdr, &e2e, 2001);
        assert!(
            matches!(dec, RemoteDecision::ApplyInline { .. }),
            "A 가 B 의 클립보드를 적용"
        );
    }

    /// 창립자 A + 조인자 B 가 같은 GK 로 릴레이에 붙은 상태를 만든다.
    async fn joined_pair() -> (
        sb_net::Connection,
        sb_net::Connection,
        SyncEngine,
        SyncEngine,
        Identity,
        Identity,
    ) {
        init_crypto();
        let server_id = Identity::generate();
        let (scert_der, _) = server_id.tls_material().unwrap();
        let server_fp = cert_fingerprint(&CertificateDer::from(scert_der));
        let shared = Shared::new(Some(sha256(b"setup-token")));
        let addr = start_server(shared, &server_id).await;

        let a = Identity::generate();
        let b = Identity::generate();

        let (genesis, wid) = wslog::build_genesis(&a, "eng-team", 1);
        let mut ca = sb_net::connect(addr, server_fp, &a).await.unwrap();
        ca.send(C2s::ClaimWorkspace {
            token: "setup-token".into(),
            genesis: wslog::entry_bytes(&genesis),
        })
        .await
        .unwrap();
        assert!(matches!(ca.recv().await.unwrap(), S2c::AppendAck { seq: 0, .. }));

        let grant = sb_crypto::generate_grant();
        let gc = wslog::build_grant_cert(&a, grant.pk_der.clone(), 10_000, wid);
        let head = wslog::entry_hash(&wslog::entry_bytes(&genesis));
        let add = wslog::build_add(&grant.sk, gc, &b.public(), head, 1, 2);
        ca.send(C2s::AppendEntry {
            entry: wslog::entry_bytes(&add),
        })
        .await
        .unwrap();
        assert!(matches!(ca.recv().await.unwrap(), S2c::AppendAck { seq: 1, .. }));

        let gk = GroupKey::from_bytes(0, [77u8; 32]);
        let ea = SyncEngine::new(a.device_id(), gk.clone(), cfg());
        let eb = SyncEngine::new(b.device_id(), gk, cfg());

        let mut cb = sb_net::connect(addr, server_fp, &b).await.unwrap();
        cb.send(C2s::Hello(Hello {
            device_id: b.device_id(),
            proto_min: sb_proto::params::PROTO_MIN,
            proto_max: sb_proto::params::PROTO_MAX,
            app_version: "test".into(),
            epoch: 0,
            log_head: (0, [0u8; 32]),
        }))
        .await
        .unwrap();
        assert!(matches!(cb.recv().await.unwrap(), S2c::Welcome(_)));

        (ca, cb, ea, eb, a, b)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// 파일 클립(`ContentKind::Files`)이 릴레이를 거쳐 이름·내용 그대로 도착하고,
    /// 서버가 보는 바이트에는 파일명이 없다.
    #[tokio::test]
    async fn file_clip_syncs_through_relay_without_leaking_names() {
        let (mut ca, mut cb, mut ea, mut eb, a, _b) = joined_pair().await;

        let bundle = sb_proto::FileBundle::new(vec![
            sb_proto::FileEntry {
                name: "보고서.pdf".into(),
                data: vec![9u8; 3000],
            },
            sb_proto::FileEntry {
                name: "notes.txt".into(),
                data: b"hello files".to_vec(),
            },
        ]);
        let plain = sb_proto::encode(&bundle).unwrap();

        let sig = match ea.on_local_clipboard(ContentKind::Files, &plain, 1000).unwrap() {
            LocalOutcome::Emit(s) => *s,
            o => panic!("Emit 기대: {o:?}"),
        };

        // 서버가 만지는 두 바이트열(신호 e2e·본문 암호문)에 파일명이 노출되지 않는다.
        assert!(!contains(&sig.e2e, "보고서.pdf".as_bytes()));
        assert!(!contains(&sig.body_ct, "notes.txt".as_bytes()));

        ca.send(C2s::ClipSignal {
            hdr: sig.hdr.clone(),
            e2e: sig.e2e.clone(),
        })
        .await
        .unwrap();

        let (origin, hdr, e2e) = recv_fanout(&mut cb).await;
        assert_eq!(origin, a.device_id());
        // 파일은 inline 대상이 아니다(텍스트만 inline) → fetch 경로를 탄다.
        match eb.on_remote_signal(origin, hdr.clone(), &e2e, 1001) {
            RemoteDecision::NeedFetch { id, kind, .. } => {
                assert_eq!(id, sig.hdr.id);
                assert_eq!(kind, ContentKind::Files);
            }
            o => panic!("NeedFetch 기대: {o:?}"),
        }

        // 보유자(A) 송신 캐시 → B 적용.
        let ct = ea.serve_content(&hdr.id, 1002).expect("송신 캐시에 본문");
        let applied = eb.on_content_fetched(hdr.id, &ct, 1003).unwrap();
        assert_eq!(applied.kind, ContentKind::Files);

        let back: sb_proto::FileBundle = sb_proto::decode(&applied.plaintext).unwrap();
        assert_eq!(back, bundle, "이름·내용이 그대로 도착");

        // 받는 쪽 히스토리 미리보기에는 파일명이 보인다(로컬 복호 후이므로).
        let newest = eb.history().list().next().unwrap();
        assert_eq!(newest.kind, ContentKind::Files);
        assert!(newest.preview.contains("보고서.pdf"), "{}", newest.preview);
    }

    #[tokio::test]
    async fn non_member_cannot_sync() {
        init_crypto();
        let server_id = Identity::generate();
        let (scert_der, _) = server_id.tls_material().unwrap();
        let server_fp = cert_fingerprint(&CertificateDer::from(scert_der));
        let shared = Shared::new(None);
        let addr = start_server(shared, &server_id).await;

        // 아무도 클레임 안 함 → 로그 비어있음 → 모두 비멤버.
        let intruder = Identity::generate();
        let mut c = sb_net::connect(addr, server_fp, &intruder).await.unwrap();
        // Hello 는 비멤버라 거부(NotMember).
        c.send(C2s::Hello(Hello {
            device_id: intruder.device_id(),
            proto_min: 2,
            proto_max: 2,
            app_version: "x".into(),
            epoch: 0,
            log_head: (0, [0u8; 32]),
        }))
        .await
        .unwrap();
        match c.recv().await.unwrap() {
            S2c::Error {
                code: ErrorCode::NotMember,
                ..
            } => {}
            other => panic!("NotMember 기대: {other:?}"),
        }
    }
}
