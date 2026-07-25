//! 식별자 타입과 LWW 정렬 키 (§5.2 / §5.3).
//!
//! 와이어 호환을 위해 계획서 §5.2의 타입 별칭을 그대로 따른다.
//! `DeviceId`/`ContentId`/`Locator`는 `[u8; 32]`, `Epoch`/`Lamport`는 `u64`.

/// 장치 신원 = SHA-256(cert SPKI). TLS 계층 신원과 동일 (D4).
pub type DeviceId = [u8; 32];

/// 콘텐츠 주소 = keyed BLAKE3(k_cid[epoch], plaintext) — D21(개정). 서버 역산 불가.
pub type ContentId = [u8; 32];

/// 그룹 키 세대(단조 증가). 서버는 번호만 안다.
pub type Epoch = u64;

/// Lamport 논리 시계.
pub type Lamport = u64;

/// 초대 blob 색인 = HKDF(K_code, "sb/inv-locator").
pub type Locator = [u8; 32];

/// 32바이트 식별자를 소문자 hex 문자열로. 로그/진단에서 앞 8바이트만 노출하는 규칙(§4.7)에 사용.
pub fn hex32(id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// 로그/진단용 짧은 식별자 — 앞 8바이트(16 hex)만 (§4.7 금지 규칙 준수).
pub fn short_id(id: &[u8; 32]) -> String {
    hex32(id)[..16].to_string()
}

/// LWW 판정 키 (§5.3): `(lamport, wall_ts_ms, device_id)` 사전식 비교.
///
/// `key(incoming) > key(current_applied)` 일 때만 로컬 클립보드에 반영한다.
/// 필드 선언 순서가 곧 비교 우선순위이므로 순서를 바꾸지 말 것.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LwwKey {
    pub lamport: Lamport,
    pub wall_ts_ms: u64,
    pub origin: DeviceId,
}

impl LwwKey {
    pub fn new(lamport: Lamport, wall_ts_ms: u64, origin: DeviceId) -> Self {
        Self {
            lamport,
            wall_ts_ms,
            origin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lww_lamport_dominates() {
        let a = LwwKey::new(5, 100, [0u8; 32]);
        let b = LwwKey::new(4, 999, [0xff; 32]);
        assert!(a > b, "더 큰 lamport가 wall_ts/id와 무관하게 이긴다");
    }

    #[test]
    fn lww_wall_ts_breaks_tie() {
        let a = LwwKey::new(5, 200, [0u8; 32]);
        let b = LwwKey::new(5, 100, [0xff; 32]);
        assert!(a > b, "동일 lamport면 나중 wall_ts가 이긴다");
    }

    #[test]
    fn lww_device_id_final_tiebreak() {
        let a = LwwKey::new(5, 100, [2u8; 32]);
        let b = LwwKey::new(5, 100, [1u8; 32]);
        assert!(a > b, "완전 동률이면 device_id로 결정적 tie-break");
    }

    #[test]
    fn short_id_is_16_hex() {
        let id = [0xabu8; 32];
        assert_eq!(short_id(&id), "abababababababab");
        assert_eq!(hex32(&id).len(), 64);
    }
}
