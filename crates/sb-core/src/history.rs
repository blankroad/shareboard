//! 인메모리 클립보드 히스토리 모델 (§7.2). 암호화 디스크 영속은 sb-store 소관.

use std::collections::VecDeque;

use sb_proto::{ContentId, ContentKind, DeviceId};

/// 항목 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Peer(DeviceId),
}

/// 히스토리 항목. 본문 전체는 보관하지 않고(메모리/보안), 미리보기만 UI로 전달(§3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: ContentId,
    pub kind: ContentKind,
    pub origin: Origin,
    pub size: u64,
    pub created_at_ms: u64,
    /// 텍스트 앞 256자 미리보기(또는 이미지 표시용 라벨). 평문 전문 아님.
    pub preview: String,
    pub pinned: bool,
}

/// 링 버퍼 히스토리. `cap` 초과 시 pinned 아닌 가장 오래된 항목부터 축출.
#[derive(Debug)]
pub struct HistoryBuffer {
    items: VecDeque<HistoryItem>,
    cap: usize,
}

impl HistoryBuffer {
    pub fn new(cap: usize) -> Self {
        Self { items: VecDeque::new(), cap: cap.max(1) }
    }

    /// 항목 추가(최신이 앞). 동일 id 존재 시 갱신·앞으로 이동(dedup).
    pub fn add(&mut self, item: HistoryItem) {
        if let Some(pos) = self.items.iter().position(|x| x.id == item.id) {
            self.items.remove(pos);
        }
        self.items.push_front(item);
        self.evict();
    }

    fn evict(&mut self) {
        while self.items.len() > self.cap {
            // 뒤(오래된)에서부터 pinned 아닌 것 제거.
            if let Some(pos) = self.items.iter().rposition(|x| !x.pinned) {
                self.items.remove(pos);
            } else {
                break; // 전부 pinned 면 상한 초과 허용.
            }
        }
    }

    pub fn list(&self) -> impl Iterator<Item = &HistoryItem> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: &ContentId) -> Option<&HistoryItem> {
        self.items.iter().find(|x| x.id == *id)
    }

    pub fn set_pinned(&mut self, id: &ContentId, pinned: bool) -> bool {
        if let Some(it) = self.items.iter_mut().find(|x| x.id == *id) {
            it.pinned = pinned;
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self, id: &ContentId) -> bool {
        if let Some(pos) = self.items.iter().position(|x| x.id == *id) {
            self.items.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// 텍스트 미리보기 생성 — 앞 256자(문자 경계 안전), 개행은 공백으로.
pub fn text_preview(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    flat.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u8, pinned: bool) -> HistoryItem {
        HistoryItem {
            id: [id; 32],
            kind: ContentKind::Text,
            origin: Origin::Local,
            size: 1,
            created_at_ms: id as u64,
            preview: format!("item{id}"),
            pinned,
        }
    }

    #[test]
    fn newest_first_and_dedup() {
        let mut h = HistoryBuffer::new(10);
        h.add(item(1, false));
        h.add(item(2, false));
        h.add(item(1, false)); // dedup → 앞으로
        assert_eq!(h.len(), 2);
        assert_eq!(h.list().next().unwrap().id, [1u8; 32]);
    }

    #[test]
    fn evicts_oldest_non_pinned() {
        let mut h = HistoryBuffer::new(2);
        h.add(item(1, true)); // pinned
        h.add(item(2, false));
        h.add(item(3, false)); // 상한 초과 → 2 축출(1은 pinned 보존)
        assert!(h.get(&[1u8; 32]).is_some());
        assert!(h.get(&[2u8; 32]).is_none());
        assert!(h.get(&[3u8; 32]).is_some());
    }

    #[test]
    fn preview_truncates_and_flattens() {
        let p = text_preview("line1\nline2");
        assert_eq!(p, "line1 line2");
        let long = "a".repeat(500);
        assert_eq!(text_preview(&long).chars().count(), 256);
    }
}
