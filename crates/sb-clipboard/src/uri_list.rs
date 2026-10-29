//! 파일 목록 ↔ Linux 클립보드 텍스트 형식 변환 (순수 함수 — 모든 플랫폼에서 테스트된다).
//!
//! Linux 파일 관리자는 파일 복사를 두 형식으로 주고받는다:
//! - `text/uri-list` (RFC 2483): `file:///경로` 를 줄마다. KDE Dolphin·GTK4 앱·대부분의 앱.
//! - `x-special/gnome-copied-files`: 첫 줄 `copy`/`cut` + URI 줄. GNOME Files(Nautilus)·Nemo·
//!   Caja·Thunar 가 붙여넣기에 쓴다. 이게 없으면 GNOME 에서 "붙여넣기"가 비활성으로 남는다.

use std::path::{Path, PathBuf};

/// 경로 → `file://` URI. 비ASCII·공백·예약 문자는 퍼센트 인코딩(바이트 단위 — 한글 파일명 OK).
pub fn path_to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path_bytes(path).iter() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `text/uri-list` 본문(CRLF 구분, RFC 2483).
pub fn to_uri_list(paths: &[PathBuf]) -> String {
    let mut s = String::new();
    for p in paths {
        s.push_str(&path_to_uri(p));
        s.push_str("\r\n");
    }
    s
}

/// `x-special/gnome-copied-files` 본문 — `copy\n<uri>\n<uri>` (끝 줄바꿈 없음, Nautilus 와 동일).
pub fn to_gnome_copied_files(paths: &[PathBuf]) -> String {
    let mut s = String::from("copy");
    for p in paths {
        s.push('\n');
        s.push_str(&path_to_uri(p));
    }
    s
}

/// `text/uri-list` 또는 `x-special/gnome-copied-files` 본문 → 로컬 파일 경로.
/// 주석(`#`)·`copy`/`cut` 헤더·`file://` 가 아닌 URI(웹 주소 등)는 건너뛴다.
pub fn parse(body: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(body);
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(uri_to_path)
        .collect()
}

/// `file:///a/b`, `file://localhost/a/b` → `/a/b`. 다른 호스트·다른 스킴은 `None`.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        // file://host/path — 로컬 호스트만 받는다.
        let (host, path) = rest.split_once('/')?;
        if host != "localhost" {
            return None;
        }
        return decode(&format!("/{path}"));
    };
    decode(path)
}

fn decode(s: &str) -> Option<PathBuf> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            // 잘린 이스케이프(`%4`)·잘못된 16진수는 깨진 URI — 그 항목을 버린다.
            if i + 2 >= b.len() {
                return None;
            }
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(bytes_to_path(out))
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}
#[cfg(not(unix))]
fn path_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(b))
}
#[cfg(not(unix))]
fn bytes_to_path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&b).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_spaces_and_korean() {
        let u = path_to_uri(Path::new("/home/me/새 문서 #1.txt"));
        assert_eq!(u, "file:///home/me/%EC%83%88%20%EB%AC%B8%EC%84%9C%20%231.txt");
    }

    #[test]
    fn uri_list_roundtrip() {
        let paths = vec![
            PathBuf::from("/tmp/a b.png"),
            PathBuf::from("/home/me/문서/보고서 (최종).pdf"),
        ];
        let body = to_uri_list(&paths);
        assert!(body.ends_with("\r\n"));
        assert_eq!(parse(body.as_bytes()), paths);
    }

    #[test]
    fn gnome_format_roundtrip() {
        let paths = vec![PathBuf::from("/tmp/x.txt"), PathBuf::from("/tmp/y z.txt")];
        let body = to_gnome_copied_files(&paths);
        assert!(body.starts_with("copy\nfile:///tmp/x.txt\n"));
        assert!(!body.ends_with('\n'));
        assert_eq!(parse(body.as_bytes()), paths, "copy 헤더는 건너뛴다");
        assert_eq!(
            parse(b"cut\nfile:///tmp/x.txt"),
            vec![PathBuf::from("/tmp/x.txt")]
        );
    }

    #[test]
    fn skips_comments_remote_and_web_uris() {
        let body = b"# comment\r\nfile://localhost/tmp/ok\r\nfile://otherhost/tmp/no\r\nhttps://example.com/a.png\r\n";
        assert_eq!(parse(body), vec![PathBuf::from("/tmp/ok")]);
    }

    #[test]
    fn bad_escape_is_dropped_not_panicking() {
        assert!(parse(b"file:///tmp/%G1").is_empty());
        assert!(parse(b"file:///tmp/%4").is_empty());
        assert!(parse(b"file:///tmp/%").is_empty());
    }
}
