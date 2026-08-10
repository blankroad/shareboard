//! 동기화 엔진 (§3.2, §3.3, §5.3). 로컬 복사 → 신호 발행, 원격 신호 → LWW 판정 → 적용.
//!
//! OS 클립보드 I/O·네트워크는 하지 않는다(sb-clipboard/sb-net 소관). 이 엔진은 순수 상태
//! 머신으로, 입력(로컬 콘텐츠/원격 신호/받은 콘텐츠)을 받아 **결정**을 반환한다 → headless 테스트 가능.

use std::collections::HashMap;
use std::collections::VecDeque;

use sb_crypto::GroupKey;
use sb_proto::params::{INLINE_THRESHOLD, SEND_CACHE_ITEMS, SEND_CACHE_TTL_S};
use sb_proto::{ContentId, ContentKind, DeviceId, Epoch, LwwKey, SignalBody, SignalHdr};

use crate::history::{HistoryBuffer, HistoryItem, Origin};
use crate::settings::EngineConfig;
use crate::{compress, CoreError};

/// 발행할 신호 + 서빙용 본문 암호문.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingSignal {
    pub hdr: SignalHdr,
    /// seal(k_sig, SignalBody).
    pub e2e: Vec<u8>,
    /// seal(k_body, content) — 송신 캐시에 보관되어 fetch 에 응답.
    pub body_ct: Vec<u8>,
}

/// 로컬 클립보드 변화 처리 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOutcome {
    /// 발행할 신호(이미 히스토리·송신 캐시에 반영됨).
    Emit(Box<OutgoingSignal>),
    /// 원격 write 의 에코 — 발행 안 함.
    Echo,
    /// 동기화 off.
    Disabled,
    /// 해당 kind 동기화 비활성.
    KindDisabled,
    /// max_content_bytes 초과 — 로컬 히스토리만.
    TooLarge,
}

/// 원격 신호 처리 결정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDecision {
    /// inline 콘텐츠 즉시 적용(caller 가 OS 클립보드에 write).
    ApplyInline {
        id: ContentId,
        kind: ContentKind,
        plaintext: Vec<u8>,
    },
    /// 콘텐츠 fetch 필요.
    NeedFetch {
        id: ContentId,
        kind: ContentKind,
        ct_size: u64,
    },
    /// 무시(사유).
    Ignore(IgnoreReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    SyncOff,
    EpochMismatch,
    OpenFailed,
    Malformed,
    OriginMismatch,
    Superseded,
}

/// fetch 로 받은 콘텐츠 적용 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedContent {
    pub id: ContentId,
    pub kind: ContentKind,
    pub plaintext: Vec<u8>,
}

struct PendingFetch {
    key: LwwKey,
    kind: ContentKind,
    origin: DeviceId,
    epoch: Epoch,
    compressed: bool,
}

/// 송신 캐시 — 최근 SEND_CACHE_ITEMS(5)개·TTL 5분. fetch 서빙용 본문 암호문.
#[derive(Default)]
struct SendCache {
    map: HashMap<ContentId, (Vec<u8>, u64)>, // id → (body_ct, expiry_ms)
    order: VecDeque<ContentId>,
}

impl SendCache {
    fn put(&mut self, id: ContentId, body_ct: Vec<u8>, now_ms: u64) {
        if self
            .map
            .insert(id, (body_ct, now_ms + SEND_CACHE_TTL_S * 1000))
            .is_none()
        {
            self.order.push_back(id);
        }
        while self.order.len() > SEND_CACHE_ITEMS {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
    fn get(&self, id: &ContentId, now_ms: u64) -> Option<&[u8]> {
        self.map
            .get(id)
            .filter(|(_, exp)| *exp > now_ms)
            .map(|(b, _)| b.as_slice())
    }
}

/// SignalBody 봉인 AAD = 캐노니컬 헤더 CBOR ‖ origin (§4.6).
fn signal_aad(hdr: &SignalHdr, origin: &DeviceId) -> Vec<u8> {
    let mut a = sb_proto::encode(hdr).expect("SignalHdr encode");
    a.extend_from_slice(origin);
    a
}

/// 콘텐츠 본문 봉인 AAD = id ‖ epoch.
fn body_aad(id: &ContentId, epoch: Epoch) -> Vec<u8> {
    let mut a = Vec::with_capacity(40);
    a.extend_from_slice(id);
    a.extend_from_slice(&epoch.to_le_bytes());
    a
}

/// 동기화 엔진.
pub struct SyncEngine {
    me: DeviceId,
    gk: GroupKey,
    cfg: EngineConfig,
    lww: crate::lww::LwwState,
    suppress: crate::suppress::SuppressSet,
    history: HistoryBuffer,
    send_cache: SendCache,
    pending: HashMap<ContentId, PendingFetch>,
}

impl SyncEngine {
    pub fn new(me: DeviceId, gk: GroupKey, cfg: EngineConfig) -> Self {
        let cap = cfg.history_cap;
        Self {
            me,
            gk,
            cfg,
            lww: crate::lww::LwwState::new(me),
            suppress: crate::suppress::SuppressSet::new(),
            history: HistoryBuffer::new(cap),
            send_cache: SendCache::default(),
            pending: HashMap::new(),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.gk.epoch()
    }

    pub fn history(&self) -> &HistoryBuffer {
        &self.history
    }
    pub fn history_mut(&mut self) -> &mut HistoryBuffer {
        &mut self.history
    }

    /// GK 회전 반영. 구 epoch pending 은 폐기.
    pub fn set_group_key(&mut self, gk: GroupKey) {
        self.gk = gk;
        self.pending.clear();
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.cfg.enabled = enabled;
    }

    /// fetch 요청에 서빙할 본문 암호문(송신 캐시).
    pub fn serve_content(&self, id: &ContentId, now_ms: u64) -> Option<Vec<u8>> {
        self.send_cache.get(id, now_ms).map(|b| b.to_vec())
    }

    fn add_history(&mut self, id: ContentId, kind: ContentKind, origin: Origin, plaintext: &[u8], now: u64) {
        let preview = match kind {
            ContentKind::Text => crate::history::text_preview(&String::from_utf8_lossy(plaintext)),
            ContentKind::ImagePng => format!("[이미지 {} bytes]", plaintext.len()),
            // 파일명은 GK 암호문 안에만 있던 값 — 로컬 UI 표시용으로만 꺼낸다.
            ContentKind::Files => match sb_proto::decode::<sb_proto::FileBundle>(plaintext) {
                Ok(b) => b.preview(),
                Err(_) => format!("[파일 {} bytes]", plaintext.len()),
            },
        };
        self.history.add(HistoryItem {
            id,
            kind,
            origin,
            size: plaintext.len() as u64,
            created_at_ms: now,
            preview,
            pinned: false,
        });
    }

    /// 로컬 클립보드 변화 (§3.2 송신 경로).
    pub fn on_local_clipboard(
        &mut self,
        kind: ContentKind,
        plaintext: &[u8],
        now_ms: u64,
    ) -> Result<LocalOutcome, CoreError> {
        let id = self.gk.content_id(plaintext);

        // 에코 억제 — 원격 write 가 로컬 워처를 발화시킨 경우.
        if self.suppress.should_suppress(&id, now_ms) {
            return Ok(LocalOutcome::Echo);
        }

        // 로컬 히스토리 기록(발행 여부와 무관).
        self.add_history(id, kind, Origin::Local, plaintext, now_ms);

        if !self.cfg.enabled {
            return Ok(LocalOutcome::Disabled);
        }
        let kind_ok = match kind {
            ContentKind::Text => self.cfg.sync_text,
            ContentKind::ImagePng => self.cfg.sync_images,
            ContentKind::Files => self.cfg.sync_files,
        };
        if !kind_ok {
            return Ok(LocalOutcome::KindDisabled);
        }
        if plaintext.len() as u64 > self.cfg.max_content_bytes {
            return Ok(LocalOutcome::TooLarge);
        }

        let lamport = self.lww.tick();
        let epoch = self.gk.epoch();
        let (body_plain, compressed) = compress::maybe_compress(kind, plaintext);
        let body_ct = self.gk.seal_body(&body_aad(&id, epoch), &body_plain)?;
        let ct_size = body_ct.len() as u64;
        let hdr = SignalHdr { id, epoch, ct_size };

        let inline = if kind == ContentKind::Text && body_plain.len() <= INLINE_THRESHOLD {
            Some(body_plain.clone())
        } else {
            None
        };
        let body = SignalBody {
            kind,
            plain_size: plaintext.len() as u64,
            lamport,
            wall_ts_ms: now_ms,
            origin: self.me,
            compressed,
            inline,
        };
        let body_bytes = sb_proto::encode(&body)?;
        let e2e = self.gk.seal_signal(&signal_aad(&hdr, &self.me), &body_bytes)?;

        self.lww.set_current(LwwKey::new(lamport, now_ms, self.me));
        self.send_cache.put(id, body_ct.clone(), now_ms);

        Ok(LocalOutcome::Emit(Box::new(OutgoingSignal { hdr, e2e, body_ct })))
    }

    /// 원격 신호 수신 (§3.3, §5.3).
    pub fn on_remote_signal(
        &mut self,
        origin: DeviceId,
        hdr: SignalHdr,
        e2e: &[u8],
        now_ms: u64,
    ) -> RemoteDecision {
        if !self.cfg.enabled {
            return RemoteDecision::Ignore(IgnoreReason::SyncOff);
        }
        if hdr.epoch != self.gk.epoch() {
            return RemoteDecision::Ignore(IgnoreReason::EpochMismatch);
        }
        let aad = signal_aad(&hdr, &origin);
        let body_plain = match self.gk.open_signal(&aad, e2e) {
            Ok(b) => b,
            Err(_) => return RemoteDecision::Ignore(IgnoreReason::OpenFailed),
        };
        let body: SignalBody = match sb_proto::decode(&body_plain) {
            Ok(b) => b,
            Err(_) => return RemoteDecision::Ignore(IgnoreReason::Malformed),
        };
        if body.origin != origin {
            return RemoteDecision::Ignore(IgnoreReason::OriginMismatch);
        }

        // 모든 수신 신호에 대해 인과 보존 max-merge.
        self.lww.merge_lamport(body.lamport);

        let key = LwwKey::new(body.lamport, body.wall_ts_ms, origin);
        if !self.lww.would_apply(key) {
            return RemoteDecision::Ignore(IgnoreReason::Superseded);
        }

        match body.inline {
            Some(inline) => {
                let content = match compress::maybe_decompress(&inline, body.compressed) {
                    Ok(c) => c,
                    Err(_) => return RemoteDecision::Ignore(IgnoreReason::Malformed),
                };
                // inline CID 검증(§5.5 — 불일치는 프로토콜 위반).
                if self.gk.content_id(&content) != hdr.id {
                    return RemoteDecision::Ignore(IgnoreReason::Malformed);
                }
                self.apply_incoming(hdr.id, body.kind, &content, key, origin, now_ms);
                RemoteDecision::ApplyInline {
                    id: hdr.id,
                    kind: body.kind,
                    plaintext: content,
                }
            }
            None => {
                self.pending.insert(
                    hdr.id,
                    PendingFetch {
                        key,
                        kind: body.kind,
                        origin,
                        epoch: hdr.epoch,
                        compressed: body.compressed,
                    },
                );
                RemoteDecision::NeedFetch {
                    id: hdr.id,
                    kind: body.kind,
                    ct_size: hdr.ct_size,
                }
            }
        }
    }

    /// fetch 로 받은 본문 암호문 적용 (§5.4 Verifying→Applying).
    pub fn on_content_fetched(
        &mut self,
        id: ContentId,
        sealed_body: &[u8],
        now_ms: u64,
    ) -> Result<AppliedContent, CoreError> {
        let p = self.pending.remove(&id).ok_or(CoreError::NoPending)?;
        let body_plain = self.gk.open_body(&body_aad(&id, p.epoch), sealed_body)?;
        let content = compress::maybe_decompress(&body_plain, p.compressed)?;
        if self.gk.content_id(&content) != id {
            return Err(CoreError::CidMismatch);
        }
        self.apply_incoming(id, p.kind, &content, p.key, p.origin, now_ms);
        Ok(AppliedContent {
            id,
            kind: p.kind,
            plaintext: content,
        })
    }

    fn apply_incoming(
        &mut self,
        id: ContentId,
        kind: ContentKind,
        content: &[u8],
        key: LwwKey,
        origin: DeviceId,
        now: u64,
    ) {
        // OS write 직전 suppress 등록(에코 차단, §5.3).
        self.suppress.register(id, now);
        if self.lww.would_apply(key) {
            self.lww.set_current(key);
        }
        self.add_history(id, kind, Origin::Peer(origin), content, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_crypto::GroupKey;

    fn engine(me: u8, gk: &GroupKey) -> SyncEngine {
        let cfg = EngineConfig {
            enabled: true,
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_content_bytes: 10 * 1024 * 1024,
            history_cap: 30,
        };
        SyncEngine::new([me; 32], gk.clone(), cfg)
    }

    #[test]
    fn local_emit_then_remote_apply_inline() {
        // 공유 GK 를 가진 두 엔진(A, B).
        let gk = GroupKey::from_bytes(1, [42u8; 32]);
        let mut a = engine(1, &gk);
        let mut b = engine(2, &gk);

        let out = a.on_local_clipboard(ContentKind::Text, b"hello", 1000).unwrap();
        let sig = match out {
            LocalOutcome::Emit(s) => *s,
            other => panic!("Emit 기대, got {other:?}"),
        };
        // 서버가 origin=A 를 스탬프해 B 에 전달.
        let dec = b.on_remote_signal([1u8; 32], sig.hdr.clone(), &sig.e2e, 1001);
        assert_eq!(
            dec,
            RemoteDecision::ApplyInline {
                id: sig.hdr.id,
                kind: ContentKind::Text,
                plaintext: b"hello".to_vec()
            }
        );
    }

    #[test]
    fn echo_is_suppressed() {
        let gk = GroupKey::from_bytes(1, [7u8; 32]);
        let mut a = engine(1, &gk);
        let mut b = engine(2, &gk);
        let sig = match a.on_local_clipboard(ContentKind::Text, b"x", 10).unwrap() {
            LocalOutcome::Emit(s) => *s,
            _ => unreachable!(),
        };
        // B 가 적용(suppress 등록됨).
        b.on_remote_signal([1u8; 32], sig.hdr.clone(), &sig.e2e, 11);
        // B 의 OS 워처가 같은 내용으로 발화 → 에코 억제.
        let out = b.on_local_clipboard(ContentKind::Text, b"x", 12).unwrap();
        assert_eq!(out, LocalOutcome::Echo);
    }

    #[test]
    fn large_content_needs_fetch_and_verifies() {
        let gk = GroupKey::from_bytes(1, [9u8; 32]);
        let mut a = engine(1, &gk);
        let mut b = engine(2, &gk);
        // 이미지는 inline 대상 아님(kind != Text) + 압축 안 함 → fetch 경로 강제.
        let img: Vec<u8> = (0..40_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let sig = match a.on_local_clipboard(ContentKind::ImagePng, &img, 100).unwrap() {
            LocalOutcome::Emit(s) => *s,
            _ => unreachable!(),
        };
        let dec = b.on_remote_signal([1u8; 32], sig.hdr.clone(), &sig.e2e, 101);
        match dec {
            RemoteDecision::NeedFetch { id, ct_size, kind } => {
                assert_eq!(id, sig.hdr.id);
                assert_eq!(kind, ContentKind::ImagePng);
                assert_eq!(ct_size, sig.body_ct.len() as u64);
                // A 가 송신 캐시에서 본문 서빙 → B 가 적용.
                let served = a.serve_content(&id, 102).unwrap();
                let applied = b.on_content_fetched(id, &served, 103).unwrap();
                assert_eq!(applied.plaintext, img);

                // 위조 본문(CID 불일치)은 거부되어야 하지만 pending 이 이미 소비됨 →
                // 새 신호로 다시 pending 을 만든 뒤 위조 본문 검증.
                let dec2 = b.on_remote_signal([1u8; 32], sig.hdr.clone(), &sig.e2e, 104);
                // 같은 항목 재수신은 Superseded(이미 적용됨).
                assert_eq!(dec2, RemoteDecision::Ignore(IgnoreReason::Superseded));
            }
            other => panic!("NeedFetch 기대, got {other:?}"),
        }
    }

    #[test]
    fn superseded_signal_ignored() {
        let gk = GroupKey::from_bytes(1, [3u8; 32]);
        let mut a = engine(1, &gk);
        let mut b = engine(2, &gk);
        // A 가 최신(lamport 큰) 항목을 B 에 적용시킨 뒤, 낮은 lamport 신호는 무시.
        let s1 = match a.on_local_clipboard(ContentKind::Text, b"first", 10).unwrap() {
            LocalOutcome::Emit(s) => *s,
            _ => unreachable!(),
        };
        let s2 = match a.on_local_clipboard(ContentKind::Text, b"second", 20).unwrap() {
            LocalOutcome::Emit(s) => *s,
            _ => unreachable!(),
        };
        // 최신(s2) 먼저 적용.
        assert!(matches!(
            b.on_remote_signal([1u8; 32], s2.hdr.clone(), &s2.e2e, 21),
            RemoteDecision::ApplyInline { .. }
        ));
        // 낡은(s1) 도착 → Superseded.
        assert_eq!(
            b.on_remote_signal([1u8; 32], s1.hdr.clone(), &s1.e2e, 22),
            RemoteDecision::Ignore(IgnoreReason::Superseded)
        );
    }

    #[test]
    fn wrong_group_key_open_fails() {
        let gk_a = GroupKey::from_bytes(1, [1u8; 32]);
        let gk_b = GroupKey::from_bytes(1, [2u8; 32]); // 다른 GK, 같은 epoch 번호
        let mut a = engine(1, &gk_a);
        let mut b = engine(2, &gk_b);
        let sig = match a.on_local_clipboard(ContentKind::Text, b"secret", 1).unwrap() {
            LocalOutcome::Emit(s) => *s,
            _ => unreachable!(),
        };
        assert_eq!(
            b.on_remote_signal([1u8; 32], sig.hdr.clone(), &sig.e2e, 2),
            RemoteDecision::Ignore(IgnoreReason::OpenFailed)
        );
    }

    #[test]
    fn sync_off_ignores_and_disables() {
        let gk = GroupKey::from_bytes(1, [5u8; 32]);
        let mut a = engine(1, &gk);
        a.set_enabled(false);
        assert_eq!(
            a.on_local_clipboard(ContentKind::Text, b"x", 1).unwrap(),
            LocalOutcome::Disabled
        );
    }
}
