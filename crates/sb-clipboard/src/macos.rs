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

/// 파일 URL 문자열 → 실제 파일시스템 경로.
///
/// **직접 파싱하면 안 된다.** Finder 는 `file:///.file/id=6571367.13007293` 같은
/// *파일 참조 URL*(volume/file id 형태)을 올리는 경우가 있고, 그 문자열을 경로로 쓰면
/// `Not a directory (os error 20)` 이 난다. `filePathURL` 로 경로 URL 로 바꿔야 한다.
/// percent-encoding(%20 등) 해석도 NSURL 이 맡는다.
fn file_url_to_path(url_str: &str) -> Option<PathBuf> {
    let url = NSURL::URLWithString(&NSString::from_str(url_str))?;
    // 참조 URL → 경로 URL. 이미 경로 URL 이면 그대로.
    let path_url = url.filePathURL().unwrap_or(url);
    let p = path_url.path()?.to_string();
    if p.is_empty() {
        return None;
    }
    Some(PathBuf::from(p))
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

/// **테스트용** — Finder 처럼 *파일 참조 URL*(`file:///.file/id=…`)을 pasteboard 에 올린다.
/// 이 형태를 우리가 해석할 수 있는지 실기기에서 확인할 때 쓴다(clip_probe --ref).
pub fn write_file_reference_urls(paths: &[PathBuf]) -> bool {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let refs: Vec<objc2::rc::Retained<NSURL>> = paths
        .iter()
        .filter_map(|p| p.to_str())
        .filter_map(|s| NSURL::fileURLWithPath(&NSString::from_str(s)).fileReferenceURL())
        .collect();
    if refs.is_empty() {
        return false;
    }
    let objs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
        refs.iter().map(|u| ProtocolObject::from_ref(&**u)).collect();
    pb.writeObjects(&NSArray::from_slice(&objs))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// 같은 파일을 가리키는가(경로 문자열은 NFD/NFC·심볼릭링크로 달라질 수 있다).
    fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => (x.ino(), x.dev()) == (y.ino(), y.dev()),
            _ => false,
        }
    }

    /// Finder 는 `file:///.file/id=…` 형태(파일 참조 URL)를 올린다. 문자열을 그대로 경로로
    /// 쓰면 `Not a directory (os error 20)` 이 나므로 반드시 경로 URL 로 바꿔야 한다.
    #[test]
    fn resolves_finder_style_file_reference_url() {
        let dir = std::env::temp_dir().join(format!("sb-refurl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("노트.md");
        std::fs::write(&file, b"# hi").unwrap();

        let url = NSURL::fileURLWithPath(&NSString::from_str(file.to_str().unwrap()));
        let reference = url.fileReferenceURL().expect("파일 참조 URL");
        let as_string = reference.absoluteString().unwrap().to_string();
        assert!(as_string.contains("/.file/id="), "{as_string}");

        let resolved = file_url_to_path(&as_string).expect("경로 복원");
        assert!(
            same_file(&resolved, &file),
            "참조 URL 이 실제 파일로 해석되어야 한다: {resolved:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 일반 경로 URL(퍼센트 인코딩 포함)도 그대로 처리된다.
    #[test]
    fn resolves_plain_file_url_with_percent_encoding() {
        let dir = std::env::temp_dir().join(format!("sb-plainurl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b 문서.md");
        std::fs::write(&file, b"x").unwrap();

        let url = NSURL::fileURLWithPath(&NSString::from_str(file.to_str().unwrap()));
        let s = url.absoluteString().unwrap().to_string();
        assert!(s.contains("%20"), "{s}");
        // macOS 는 경로를 NFD 로 돌려주므로 문자열 비교 대신 같은 파일인지(inode) 확인한다.
        let resolved = file_url_to_path(&s).unwrap();
        assert!(same_file(&resolved, &file), "{resolved:?} vs {file:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
