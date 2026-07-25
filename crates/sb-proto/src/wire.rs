//! 와이어 메시지 (§5.2). 클라↔서버 프레임 payload.
//!
//! 전송 계층(TLS) 위에 CBOR로 인코딩된 `Envelope<C2s>` / `Envelope<S2c>` 가 오간다.
//! E2E 계층 payload(GK 암호문)는 여기서 불투명 `Vec<u8>` 로만 실린다.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ids::{ContentId, DeviceId, Epoch, Locator};
use crate::kinds::{AbortReason, AppendRejectReason, ByeReason, ErrorCode, RejectReason};
use crate::ProtoError;

/// 버전 태그를 붙인 봉투. `Envelope<C2s>` 또는 `Envelope<S2c>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<M> {
    pub v: u16,
    pub msg: M,
}

impl<M> Envelope<M> {
    pub fn new(msg: M) -> Self {
        Self {
            v: crate::params::PROTO_MAX,
            msg,
        }
    }
}

/// 평문 헤더 — 서버가 보는 전부 (D31). origin·kind·정렬키·inline은 여기 없음.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalHdr {
    /// keyed — 서버 역산 불가.
    pub id: ContentId,
    /// stale-key 감지 + 서버 캐시 무효화 기준.
    pub epoch: Epoch,
    /// fetch 계획/캐시 상한 판단.
    pub ct_size: u64,
}

/// 회전/조인 wrap 1건 — 특정 수신자(`to`) 앞 봉인.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyUpdate {
    pub to: DeviceId,
    pub epoch: Epoch,
    /// `RotationBlob` 을 X25519 ECDH-ES로 봉인한 암호문.
    pub wrap: Vec<u8>,
}

/// presence 한 항목 — 멤버 식별용. `addr` 는 서버가 스탬프한 접속 주소(metadata, blind relay 는
/// 어차피 IP 를 보므로 origin 스탬프와 동일 선상). 사내망에서 사람 식별에 사용.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub device_id: DeviceId,
    pub online: bool,
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub enc_profile: Option<Vec<u8>>,
}

/// 세션 수립 (Member lane, mTLS 확립 직후) (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// TLS cert 지문과 일치 강제 (§4.5).
    pub device_id: DeviceId,
    pub proto_min: u16,
    pub proto_max: u16,
    pub app_version: String,
    /// 보유 최신 epoch — 서버가 밀린 wrap 배달 판단.
    pub epoch: Epoch,
    /// 보유 로그 (seq, hash) — 서버가 tail 전송 판단.
    pub log_head: (u64, [u8; 32]),
}

/// Hello 응답 — v1.1 HelloAck 대체 (§5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub chosen_version: u16,
    pub epoch: Epoch,
    /// Hello.log_head 이후 엔트리(클라가 체인 검증).
    pub log_tail: Vec<Vec<u8>>,
    /// 오프라인 중 회전/조인 wrap(본인 몫 최신 1개).
    #[serde(default)]
    pub pending_key_update: Option<Vec<u8>>,
    /// 전 멤버 + 온라인 + 주소 + enc_profile.
    pub presence: Vec<PresenceEntry>,
    /// 최근 HEAD_CACHE_DEPTH(4)건 signal — 클라가 복호 후 LWW로 최신 1건 판정.
    pub head: Vec<(DeviceId, SignalHdr, Vec<u8>)>,
    /// 진단 전용(시계 오차 로그) — 판정 사용 금지.
    pub server_time_ms: u64,
}

// ───────────────────────── 클라 → 서버 ─────────────────────────
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum C2s {
    /// 세션 수립 (Member lane).
    Hello(Hello),

    // ── Guest lane (미등록 cert — 아래 4종 외 수신 시 즉시 종료) ──
    /// 부트스트랩(§4.3.2): setup 토큰 + Genesis 바이트열.
    ClaimWorkspace {
        token: String,
        genesis: Vec<u8>,
    },
    GetInviteBlob {
        locator: Locator,
    },
    /// Member lane에서도 tail 동기화에 사용.
    GetLog {
        from_seq: u64,
    },
    /// 저장 바이트열 그대로. Guest는 Add만, Member는 Remove/Epoch/RotateKem도.
    AppendEntry {
        entry: Vec<u8>,
    },

    // ── 초대 관리 (Member) ──
    /// blob ≤ 4KiB, TTL ≤ 24h.
    PutInvite {
        locator: Locator,
        blob: Vec<u8>,
        ttl_s: u32,
    },
    RevokeInvite {
        locator: Locator,
    },

    // ── 키 배포 (Member) ──
    PutKeyUpdate {
        updates: Vec<KeyUpdate>,
    },

    // ── 동기화 (Member) ──
    /// e2e = seal(k_sig, SignalBody, aad = hdr‖origin).
    ClipSignal {
        hdr: SignalHdr,
        e2e: Vec<u8>,
    },
    /// 서버가 소스(캐시/원본) 결정.
    ContentRequest {
        id: ContentId,
        epoch: Epoch,
    },
    /// ContentPull 응답.
    ContentBegin {
        id: ContentId,
        ct_size: u64,
        chunk_count: u32,
        chunk_size: u32,
    },
    /// 암호문 조각.
    ContentChunk {
        id: ContentId,
        index: u32,
        data: Vec<u8>,
    },
    ContentAbort {
        id: ContentId,
        reason: AbortReason,
    },
    /// origin의 Pull 거절.
    ContentReject {
        id: ContentId,
        reason: RejectReason,
    },

    // ── 기타 ──
    /// GK 봉인 {name, platform, log_head_hash, seq, epoch, ts}.
    SetProfile {
        epoch: Epoch,
        e2e: Vec<u8>,
    },
    /// 자발 탈퇴(잔존 멤버 UI가 회전 권고).
    Leave,
    Ping {
        nonce: u64,
    },
    Bye {
        reason: ByeReason,
    },
}

// ───────────────────────── 서버 → 클라 ─────────────────────────
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum S2c {
    Welcome(Welcome),

    // ── Guest 응답 ──
    /// None = 미존재/만료(구분 없음 — 정보 최소화).
    InviteBlob {
        blob: Option<Vec<u8>>,
    },
    LogEntries {
        entries: Vec<Vec<u8>>,
        done: bool,
    },
    AppendAck {
        seq: u64,
        head_hash: [u8; 32],
    },
    AppendReject {
        reason: AppendRejectReason,
    },

    // ── 동기화 ──
    /// origin은 서버가 인증 세션에서 스탬프 — 발신자에게는 미반송.
    SignalFanout {
        origin: DeviceId,
        hdr: SignalHdr,
        e2e: Vec<u8>,
    },
    /// → origin: 업로드 요청.
    ContentPull {
        id: ContentId,
    },
    ContentBegin {
        id: ContentId,
        ct_size: u64,
        chunk_count: u32,
        chunk_size: u32,
    },
    ContentChunk {
        id: ContentId,
        index: u32,
        data: Vec<u8>,
    },
    ContentReject {
        id: ContentId,
        reason: RejectReason,
    },
    ContentAbort {
        id: ContentId,
        reason: AbortReason,
    },

    // ── 멤버십/키/presence ──
    /// 신규 엔트리 실시간 전파(체인 검증은 클라).
    LogAppended {
        entry: Vec<u8>,
        seq: u64,
    },
    /// 본인 몫 wrap 즉시 push.
    KeyUpdatePush {
        wrap: Vec<u8>,
    },
    Presence {
        device_id: DeviceId,
        online: bool,
        #[serde(default)]
        addr: Option<String>,
        enc_profile: Option<Vec<u8>>,
    },
    /// 본인 제거 "힌트"(인증 안 됨) — §5.4. 키 파기·crypto-erase 금지.
    Revoked,

    // ── 기타 ──
    Pong {
        nonce: u64,
    },
    Error {
        code: ErrorCode,
        detail: String,
    },
    Bye {
        reason: ByeReason,
    },
}

/// CBOR 인코딩. 프레이밍 계층(LengthDelimitedCodec)에 넣기 전 payload 바이트.
pub fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ProtoError> {
    let mut buf = Vec::new();
    ciborium::into_writer(v, &mut buf).map_err(|e| ProtoError::Encode(e.to_string()))?;
    Ok(buf)
}

/// CBOR 디코딩.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtoError> {
    ciborium::from_reader(bytes).map_err(|e| ProtoError::Decode(e.to_string()))
}

/// `Envelope` 로 감싸 인코딩.
pub fn encode_env<M: Serialize>(msg: M) -> Result<Vec<u8>, ProtoError> {
    encode(&Envelope::new(msg))
}

/// `Envelope` 를 디코딩하고 버전 지원 창(직전 1개)을 검사한다 (§5.1).
pub fn decode_env<M: DeserializeOwned>(bytes: &[u8]) -> Result<M, ProtoError> {
    let env: Envelope<M> = decode(bytes)?;
    if env.v < crate::params::PROTO_MIN || env.v > crate::params::PROTO_MAX {
        return Err(ProtoError::Version { got: env.v });
    }
    Ok(env.msg)
}
