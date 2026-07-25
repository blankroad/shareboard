//! macOS 네이티브 클립보드 감지 (§6 D8, §4.6 D17).
//!
//! `NSPasteboard.changeCount` 는 클립보드가 바뀔 때마다 증가하는 정수다. 이 정수만 비교하면
//! **콘텐츠를 폴링하지 않고** 변경을 감지할 수 있다(카운터가 바뀐 순간에만 실제 read). 순수
//! 이벤트 API 는 macOS 에 없으므로 이것이 "폴링 금지"의 실질 구현이다.
//!
//! concealed 감지: pasteboard 타입 목록에 `org.nspasteboard.ConcealedType`/`TransientType`
//! 마커가 있으면 비밀번호 매니저 등 민감 콘텐츠로 간주해 동기화에서 제외한다.

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

use crate::{ChangeWatcher, ClipContent, ClipError, ClipboardAccess};

const CONCEALED_TYPES: &[&str] = &["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

/// 현재 pasteboard changeCount.
pub fn change_count() -> isize {
    let pb = NSPasteboard::generalPasteboard();
    pb.changeCount()
}

/// 현재 클립보드가 concealed/transient 마커를 가지고 있는가.
pub fn is_concealed_now() -> bool {
    let pb = NSPasteboard::generalPasteboard();
    let Some(types) = pb.types() else { return false };
    for marker in CONCEALED_TYPES {
        let target = NSString::from_str(marker);
        for t in types.iter() {
            if t.isEqualToString(&target) {
                return true;
            }
        }
    }
    false
}

/// changeCount 기반 watcher. 카운터가 바뀐 순간에만 `access` 로 콘텐츠를 읽는다.
pub struct ChangeCountWatcher<A: ClipboardAccess> {
    access: A,
    last: isize,
}

impl<A: ClipboardAccess> ChangeCountWatcher<A> {
    pub fn new(access: A) -> Self {
        Self {
            access,
            last: change_count(),
        }
    }
}

impl<A: ClipboardAccess> ChangeWatcher for ChangeCountWatcher<A> {
    fn poll(&mut self) -> Result<Option<ClipContent>, ClipError> {
        let c = change_count();
        if c != self.last {
            self.last = c;
            self.access.read() // 정수 비교로 변경 확인된 경우에만 실제 read (D8)
        } else {
            Ok(None)
        }
    }

    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError> {
        self.access.write(content)?;
        self.last = change_count(); // 우리 쓰기로 오른 카운터 흡수(에코 억제)
        Ok(())
    }

    fn note_written(&mut self, _content: &ClipContent) {
        self.last = change_count();
    }

    fn is_concealed(&self) -> bool {
        is_concealed_now()
    }
}
