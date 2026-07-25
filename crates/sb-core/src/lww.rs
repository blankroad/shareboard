//! LWW 순서 판정과 Lamport 시계 (§5.3).

use sb_proto::{DeviceId, Lamport, LwwKey};

/// 마지막으로 적용된 항목의 LWW 키와 Lamport 시계 상태.
#[derive(Debug, Clone)]
pub struct LwwState {
    lamport: Lamport,
    current: Option<LwwKey>,
    me: DeviceId,
}

impl LwwState {
    pub fn new(me: DeviceId) -> Self {
        Self { lamport: 0, current: None, me }
    }

    pub fn lamport(&self) -> Lamport {
        self.lamport
    }

    pub fn current(&self) -> Option<LwwKey> {
        self.current
    }

    /// 로컬 복사 시 Lamport +1 후 반환 (§5.3).
    pub fn tick(&mut self) -> Lamport {
        self.lamport += 1;
        self.lamport
    }

    /// 원격 수신 시 max-merge (인과 순서 보존).
    pub fn merge_lamport(&mut self, remote: Lamport) {
        if remote > self.lamport {
            self.lamport = remote;
        }
    }

    /// `key` 가 현재 적용본보다 더 최신인가? (적용 여부 판정)
    pub fn would_apply(&self, key: LwwKey) -> bool {
        match self.current {
            None => true,
            Some(cur) => key > cur,
        }
    }

    /// 현재 적용본 갱신.
    pub fn set_current(&mut self, key: LwwKey) {
        self.current = Some(key);
    }

    pub fn me(&self) -> DeviceId {
        self.me
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_tick_monotonic() {
        let mut s = LwwState::new([1u8; 32]);
        assert_eq!(s.tick(), 1);
        assert_eq!(s.tick(), 2);
    }

    #[test]
    fn merge_takes_max() {
        let mut s = LwwState::new([1u8; 32]);
        s.tick(); // 1
        s.merge_lamport(5);
        assert_eq!(s.lamport(), 5);
        assert_eq!(s.tick(), 6, "merge 후 tick 은 6");
    }

    #[test]
    fn would_apply_by_key() {
        let mut s = LwwState::new([1u8; 32]);
        assert!(s.would_apply(LwwKey::new(1, 0, [0u8; 32])), "최초는 항상 적용");
        s.set_current(LwwKey::new(3, 100, [1u8; 32]));
        assert!(!s.would_apply(LwwKey::new(3, 100, [1u8; 32])), "동일 키는 미적용");
        assert!(s.would_apply(LwwKey::new(4, 0, [0u8; 32])), "더 큰 lamport 적용");
        assert!(!s.would_apply(LwwKey::new(2, 999, [9u8; 32])), "낮은 lamport 미적용");
    }
}
