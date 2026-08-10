//! 받은 파일 실체화 — [`FileBundle`] → 디스크(`<data_dir>/received`) → 클립보드에 올릴 경로.
//!
//! 붙여넣기는 **디스크에 실체가 있는 파일**만 가능하므로, 파일 클립을 적용할 때는 먼저 파일을
//! 만들어야 한다. 보낸 쪽 파일명은 신뢰하지 않는다([`sb_proto::safe_file_name`] 통과 필수).

use std::path::{Path, PathBuf};

use sb_proto::FileBundle;

/// 받은 파일이 놓이는 폴더.
pub fn received_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("received")
}

/// 번들을 디스크에 쓰고 만들어진 경로들을 반환한다.
///
/// - 같은 이름·같은 내용이 이미 있으면 그 파일을 재사용한다(중복 축적·에코 유발 방지).
/// - 이름이 같고 내용이 다르면 `이름 (2).ext` 로 피한다.
/// - `.part` 임시 파일에 쓰고 rename — 반쯤 쓰인 파일이 클립보드에 노출되지 않게.
/// - `mark` 면 "다른 기기에서 왔음" 표시를 남긴다.
pub fn materialize(data_dir: &Path, bundle: &FileBundle, mark: bool) -> std::io::Result<Vec<PathBuf>> {
    let dir = received_dir(data_dir);
    std::fs::create_dir_all(&dir)?;

    let mut out = Vec::with_capacity(bundle.files.len());
    for f in &bundle.files {
        let name = sb_proto::safe_file_name(&f.name);
        let path = target_path(&dir, &name, &f.data);
        if path.exists() {
            out.push(path); // 동일 내용 재사용
            continue;
        }
        let tmp = dir.join(format!(".{name}.part"));
        std::fs::write(&tmp, &f.data)?;
        std::fs::rename(&tmp, &path)?;
        if mark {
            mark_from_network(&path);
        }
        out.push(path);
    }
    Ok(out)
}

/// `name` 을 쓸 경로. 같은 내용이 이미 있으면 그 경로, 충돌하면 ` (n)` 접미사.
fn target_path(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let first = dir.join(name);
    match std::fs::read(&first) {
        Ok(existing) if existing == data => return first, // 동일 파일
        Err(_) => return first,                           // 없음 → 그대로 쓴다
        Ok(_) => {}                                       // 이름 충돌(내용 다름)
    }
    let (stem, ext) = split_name(name);
    for i in 2..1000 {
        let cand = dir.join(match &ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        });
        match std::fs::read(&cand) {
            Ok(existing) if existing == data => return cand,
            Err(_) => return cand,
            Ok(_) => {}
        }
    }
    // 1000개까지 충돌하는 병적인 경우 — 덮어쓴다(가장 오래된 사본이 밀려난다).
    first
}

/// "a.tar.gz" → ("a.tar", Some("gz")). 확장자 없으면 (name, None).
fn split_name(name: &str) -> (String, Option<String>) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (name.to_string(), None),
    }
}

/// 다른 기기에서 온 파일 표시 — macOS quarantine xattr / Windows Zone.Identifier.
/// 실패는 무시한다(표시가 없어도 파일은 정상).
fn mark_from_network(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        // 0083 = kLSQuarantineTypeOtherDownload 계열 플래그. Gatekeeper 가 실행형 파일을 검사한다.
        let _ = std::process::Command::new("xattr")
            .args(["-w", "com.apple.quarantine", "0083;00000000;shareboard;"])
            .arg(path)
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        let mut ads = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_proto::FileEntry;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sb-recv-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn bundle(name: &str, data: &[u8]) -> FileBundle {
        FileBundle::new(vec![FileEntry {
            name: name.into(),
            data: data.to_vec(),
        }])
    }

    #[test]
    fn writes_files_into_received_dir() {
        let d = tmp_dir("write");
        let paths = materialize(&d, &bundle("a.txt", b"hello"), false).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], received_dir(&d).join("a.txt"));
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"hello");
        // .part 임시 파일이 남지 않는다.
        let leftovers: Vec<_> = std::fs::read_dir(received_dir(&d))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn same_name_same_content_reuses_path() {
        let d = tmp_dir("reuse");
        let p1 = materialize(&d, &bundle("a.txt", b"same"), false).unwrap();
        let p2 = materialize(&d, &bundle("a.txt", b"same"), false).unwrap();
        assert_eq!(p1, p2, "동일 내용은 같은 경로 재사용");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn same_name_different_content_gets_suffix() {
        let d = tmp_dir("suffix");
        let p1 = materialize(&d, &bundle("a.txt", b"one"), false).unwrap();
        let p2 = materialize(&d, &bundle("a.txt", b"two"), false).unwrap();
        assert_ne!(p1, p2);
        assert_eq!(p2[0].file_name().unwrap(), "a (2).txt");
        assert_eq!(std::fs::read(&p1[0]).unwrap(), b"one");
        assert_eq!(std::fs::read(&p2[0]).unwrap(), b"two");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn hostile_names_cannot_escape_the_folder() {
        let d = tmp_dir("escape");
        let paths = materialize(&d, &bundle("../../evil.sh", b"x"), false).unwrap();
        assert_eq!(paths[0], received_dir(&d).join("evil.sh"));
        assert!(paths[0].starts_with(received_dir(&d)));
        std::fs::remove_dir_all(&d).ok();
    }
}
