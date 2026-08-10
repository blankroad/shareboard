//! macOS 네이티브 클립보드 감지 (§6 D8, §4.6 D17).
//!
//! `NSPasteboard.changeCount` 는 클립보드가 바뀔 때마다 증가하는 정수다. 이 정수만 비교하면
//! **콘텐츠를 폴링하지 않고** 변경을 감지할 수 있다(카운터가 바뀐 순간에만 실제 read). 순수
//! 이벤트 API 는 macOS 에 없으므로 이것이 "폴링 금지"의 실질 구현이다.
//!
//! concealed 감지: pasteboard 타입 목록에 `org.nspasteboard.ConcealedType`/`TransientType`
//! 마커가 있으면 비밀번호 매니저 등 민감 콘텐츠로 간주해 동기화에서 제외한다.

use std::path::PathBuf;

use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSString, NSURL};

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

/// `file:///a%20b/c.txt` → `/a b/c.txt`. 실패하면 None.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // 호스트 부분(보통 비어 있음)을 건너뛰고 경로만 취한다.
    let path_part = match rest.find('/') {
        Some(i) => &rest[i..],
        None => return None,
    };
    let decoded = percent_decode(path_part);
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded))
}

/// 최소 percent-decoding(파일 URL 경로용).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 클립보드의 파일 URL 목록 → 경로. 파일 클립이 아니면 빈 Vec.
pub fn read_file_paths() -> Vec<PathBuf> {
    let pb = NSPasteboard::generalPasteboard();
    let ty = unsafe { NSPasteboardTypeFileURL };
    let mut out = Vec::new();
    let Some(items) = pb.pasteboardItems() else {
        return out;
    };
    for item in items.iter() {
        if let Some(s) = item.stringForType(ty) {
            let raw = s.to_string();
            match file_url_to_path(&raw) {
                Some(p) => out.push(p),
                // URL 을 경로로 못 바꾼 경우 — 원본 문자열이 있어야 원인을 알 수 있다.
                None => tracing::warn!("파일 URL 을 경로로 변환 실패: {raw}"),
            }
        }
    }
    out
}

/// 경로들을 파일 URL 로 pasteboard 에 올린다(Finder 붙여넣기 가능). 성공 여부 반환.
pub fn write_file_paths(paths: &[PathBuf]) -> bool {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let urls: Vec<objc2::rc::Retained<NSURL>> = paths
        .iter()
        .filter_map(|p| p.to_str())
        .map(|s| NSURL::fileURLWithPath(&NSString::from_str(s)))
        .collect();
    if urls.is_empty() {
        return false;
    }
    let objs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
        urls.iter().map(|u| ProtocolObject::from_ref(&**u)).collect();
    pb.writeObjects(&NSArray::from_slice(&objs))
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

    fn write_file_paths(&mut self, paths: &[PathBuf]) -> Result<(), ClipError> {
        self.access.write_file_paths(paths)?;
        self.last = change_count(); // 우리 쓰기로 오른 카운터 흡수
        Ok(())
    }

    fn note_written(&mut self, _content: &ClipContent) {
        self.last = change_count();
    }

    fn is_concealed(&self) -> bool {
        is_concealed_now()
    }
}
