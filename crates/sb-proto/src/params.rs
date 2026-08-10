//! 프로토콜 상수 (§5.6 기본 파라미터 표). 설정으로 바뀌는 값은 "_DEFAULT" 접미사.

// ── 버전 ──
pub const PROTO_MIN: u16 = 2;
/// 3 = 파일 클립보드(`ContentKind::Files`) 추가. MIN 은 2 로 남겨 구 버전과도 접속은 되지만,
/// 구 버전 클라는 Files 신호를 복호하지 못해 무시한다(텍스트·이미지는 정상).
pub const PROTO_MAX: u16 = 3;

// ── 프레이밍 (§5.1) ──
/// Member lane 최대 프레임.
pub const MAX_FRAME: usize = 256 * 1024;
/// Guest lane 최대 프레임 (더 엄격).
pub const GUEST_MAX_FRAME: usize = 32 * 1024;

// ── 콘텐츠 크기 ──
/// inline 임계 — 이하 텍스트는 SignalBody에 동봉(압축 후 기준).
pub const INLINE_THRESHOLD: usize = 32 * 1024;
/// 암호문 청크 크기.
pub const CHUNK_SIZE: usize = 64 * 1024;
/// 전파 상한 기본값(설정 1–100MiB).
pub const MAX_CONTENT_SIZE_DEFAULT: u64 = 10 * 1024 * 1024;
pub const MAX_CONTENT_SIZE_MIN: u64 = 1 * 1024 * 1024;
pub const MAX_CONTENT_SIZE_MAX: u64 = 100 * 1024 * 1024;
/// 한 클립에 담을 파일 개수 상한 — 초과 시 파일 클립 자체를 건너뛴다.
pub const MAX_FILES_PER_CLIP: usize = 50;
/// 로컬 read 절대 상한 — 초과 시 read 자체 중단(§3.2).
pub const READ_HARD_LIMIT: u64 = 32 * 1024 * 1024;
/// zstd 압축 임계(텍스트, **암호화 전**).
pub const ZSTD_THRESHOLD: usize = 4 * 1024;

// ── 타이밍 ──
/// 클립보드 연타 코얼레싱.
pub const DEBOUNCE_MS: u64 = 150;
/// 송신 캐시 항목 수 / TTL.
pub const SEND_CACHE_ITEMS: usize = 5;
pub const SEND_CACHE_TTL_S: u64 = 300;
/// suppress set 유예 창(원격 write 후 로컬 워처 발화 흡수).
pub const SUPPRESS_GRACE_S: u64 = 2;
/// heartbeat idle Ping / 사망 판정 / 서버 세션 정리.
pub const HEARTBEAT_IDLE_S: u64 = 15;
pub const DEAD_S: u64 = 45;
pub const SERVER_SESSION_IDLE_S: u64 = 90;
/// 재연결 백오프 (조정 사항 4: 상한 30s).
pub const BACKOFF_START_S: u64 = 1;
pub const BACKOFF_MAX_S: u64 = 30;
pub const BACKOFF_JITTER: f64 = 0.20;
/// 핸드셰이크/인증 타임아웃.
pub const HANDSHAKE_TIMEOUT_S: u64 = 3;
pub const AUTH_TIMEOUT_S: u64 = 10;
/// fetch 타임아웃(첫 청크·청크 간).
pub const FETCH_TIMEOUT_S: u64 = 10;

// ── 레이트/캐시 ──
/// signal 상한(클라당) — 서버 강제 + 클라 자율.
pub const SIGNAL_RATE_PER_S: u32 = 10;
/// 보유자→서버 1회 업로드 bounded 큐(청크 수).
pub const UPLOAD_QUEUE_CHUNKS: usize = 32;
/// head signal 캐시 깊이.
pub const HEAD_CACHE_DEPTH: usize = 4;
/// head 본문 캐시 총량 상한(≤4×max_content_bytes, LRU 축출, 메모리 전용 — D29/§5.4).
pub const CONTENT_CACHE_BYTES_DEFAULT: u64 = 4 * MAX_CONTENT_SIZE_DEFAULT;

// ── 초대 (§4.3, §4.3.6) ──
/// Crockford Base32 12자, 60-bit 엔트로피.
pub const INVITE_CODE_CHARS: usize = 12;
pub const INVITE_CODE_BITS: u32 = 60;
pub const INVITE_TTL_DEFAULT_S: u32 = 3600;
pub const INVITE_TTL_MAX_S: u32 = 24 * 3600;
pub const INVITE_BLOB_MAX: usize = 4 * 1024;
/// Argon2id 파라미터 (m=64MiB, t=3, p=1, ~0.5s).
pub const ARGON2_MEM_KIB: u32 = 64 * 1024;
pub const ARGON2_TIME: u32 = 3;
pub const ARGON2_PAR: u32 = 1;

// ── epoch grace (§4.4) ──
pub const EPOCH_GRACE_MANUAL_S: u64 = 600;
pub const EPOCH_GRACE_REVOKE_S: u64 = 0;

// ── Guest lane 상한 (§4.5) ──
pub const GUEST_MAX_TOTAL: usize = 8;
pub const GUEST_MAX_PER_IP: usize = 1;
pub const GUEST_TTL_S: u64 = 60;
pub const GUEST_NEW_PER_MIN: u32 = 3;
pub const GUEST_GETINVITE_MAX: u32 = 5;

// ── 암호 프레이밍 ──
/// XChaCha20-Poly1305 nonce 길이(24B 랜덤 전치).
pub const NONCE_LEN: usize = 24;

// ── 서버 ──
/// 기본 리슨 포트(server.toml `bind_addr`).
pub const DEFAULT_SERVER_PORT: u16 = 45871;
pub const DEFAULT_HEALTH_PORT: u16 = 45872;
/// 서버 동시 연결 상한(초과 시 Busy 거부, 큐잉 없음).
pub const MAX_CONNECTIONS_DEFAULT: usize = 64;
pub const MAX_GUEST_CONNECTIONS_DEFAULT: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_ordering_sane() {
        assert!(MAX_CONTENT_SIZE_MIN <= MAX_CONTENT_SIZE_DEFAULT);
        assert!(MAX_CONTENT_SIZE_DEFAULT <= MAX_CONTENT_SIZE_MAX);
        assert!(READ_HARD_LIMIT >= MAX_CONTENT_SIZE_MAX / 4);
        assert!(GUEST_MAX_FRAME < MAX_FRAME);
        // proto 3(파일 클립)부터 MIN < MAX — 구 버전(2)과도 접속은 되고, Files 만 무시된다.
        assert!(PROTO_MIN <= PROTO_MAX);
        assert_eq!(PROTO_MAX, 3);
    }
}
