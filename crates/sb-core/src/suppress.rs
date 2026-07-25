//! 에코 방지 (§5.3). 원격 콘텐츠를 OS 클립보드에 쓰기 직전 ContentId 를 등록하고,
//! 이후 로컬 워처가 같은 내용으로 발화하면 유예 창 내에서 소비(재발행 차단)한다.
//!
//! 추가로 recent-hash LRU(선택)로 재복사 핑퐁을 완화한다(§5.3, 필수→선택 강등).

use std::collections::{HashMap, VecDeque};

use sb_proto::params::SUPPRESS_GRACE_S;
use sb_proto::ContentId;

/// suppress 집합 + recent LRU.
#[derive(Debug, Default)]
pub struct SuppressSet {
    /// id → 만료 시각(ms).
    grace: HashMap<ContentId, u64>,
    /// 최근 적용/발행 id LRU (기본 16).
    recent: VecDeque<ContentId>,
    recent_cap: usize,
}

impl SuppressSet {
    pub fn new() -> Self {
        Self { grace: HashMap::new(), recent: VecDeque::new(), recent_cap: 16 }
    }

    /// OS 클립보드에 원격 콘텐츠를 쓰기 직전 호출. 유예 창(2s) 등록.
    pub fn register(&mut self, id: ContentId, now_ms: u64) {
        self.grace.insert(id, now_ms + SUPPRESS_GRACE_S * 1000);
        self.touch_recent(id);
    }

    /// 로컬 워처 발화 시 호출 — 억제 대상이면 true(발행 안 함). 매칭 시 소비하지 않고
    /// 유예 창 동안 반복 매칭을 허용한다(Windows 다중 이벤트 대비, §5.3).
    pub fn should_suppress(&mut self, id: &ContentId, now_ms: u64) -> bool {
        self.gc(now_ms);
        self.grace.contains_key(id)
    }

    /// recent LRU 에 있는가(디듀프 보조).
    pub fn in_recent(&self, id: &ContentId) -> bool {
        self.recent.contains(id)
    }

    fn touch_recent(&mut self, id: ContentId) {
        if let Some(pos) = self.recent.iter().position(|x| *x == id) {
            self.recent.remove(pos);
        }
        self.recent.push_back(id);
        while self.recent.len() > self.recent_cap {
            self.recent.pop_front();
        }
    }

    /// 만료 항목 정리.
    pub fn gc(&mut self, now_ms: u64) {
        self.grace.retain(|_, exp| *exp > now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_within_grace() {
        let mut s = SuppressSet::new();
        let id = [7u8; 32];
        s.register(id, 1_000);
        assert!(s.should_suppress(&id, 1_500), "유예 창 내 억제");
        assert!(s.should_suppress(&id, 2_999), "2s 직전 억제");
    }

    #[test]
    fn expires_after_grace() {
        let mut s = SuppressSet::new();
        let id = [7u8; 32];
        s.register(id, 1_000);
        assert!(!s.should_suppress(&id, 3_001), "2s 경과 후 해제");
    }

    #[test]
    fn unrelated_not_suppressed() {
        let mut s = SuppressSet::new();
        s.register([1u8; 32], 0);
        assert!(!s.should_suppress(&[2u8; 32], 100));
    }

    #[test]
    fn recent_lru_caps() {
        let mut s = SuppressSet::new();
        for i in 0..20u8 {
            s.register([i; 32], 0);
        }
        assert!(!s.in_recent(&[0u8; 32]), "오래된 항목은 밀려남");
        assert!(s.in_recent(&[19u8; 32]));
    }
}
