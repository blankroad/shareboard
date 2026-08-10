//! 파일 클립보드 브리지 — OS 클립보드의 파일 목록 ↔ [`sb_proto::FileBundle`] 바이트.
//!
//! 읽기: 클립보드에 파일 URL 이 있으면 경로를 얻어 **크기를 먼저 stat** 하고, 상한 안일 때만
//! 실제로 읽는다(§3.2 — 큰 콘텐츠는 read 자체를 하지 않는다). 쓰기는 앱이 파일을 디스크에
//! 만든 뒤 그 경로들을 클립보드에 올리는 순서다(파일 없이 붙여넣기는 성립하지 않는다).
//!
//! 플랫폼: macOS(`public.file-url`) · Windows(`CF_HDROP`). Linux 는 아직 미지원 —
//! X11 `text/uri-list` 는 후속 과제이며, 그때까지 파일 클립은 무시된다(텍스트·이미지는 정상).

use std::path::{Path, PathBuf};

use sb_proto::params::{MAX_FILES_PER_CLIP, READ_HARD_LIMIT};
use sb_proto::{FileBundle, FileEntry};

use crate::{ClipContent, ClipError};

/// 현재 클립보드의 파일 경로 목록. 파일 클립이 아니거나 미지원 플랫폼이면 빈 Vec.
pub fn clipboard_file_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::read_file_paths()
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::read_file_paths()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// 경로 목록을 클립보드에 올린다 — 이후 Finder/탐색기에 그대로 붙여넣을 수 있다.
pub fn set_clipboard_file_paths(paths: &[PathBuf]) -> Result<(), ClipError> {
    #[cfg(target_os = "macos")]
    {
        if crate::macos::write_file_paths(paths) {
            Ok(())
        } else {
            Err(ClipError::Access("pasteboard 파일 URL 쓰기 실패".into()))
        }
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::write_file_paths(paths)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = paths;
        Err(ClipError::Unsupported)
    }
}

/// 이 플랫폼이 파일 클립보드를 지원하는가(UI 안내용).
pub const fn files_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

fn mib(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.0)
}

/// 경로 목록 → `Files` 클립 콘텐츠.
///
/// 건너뛸 때는 `ClipError::Skipped(사용자에게 보여줄 이유)` 를 돌려준다 — 파일 0개 ·
/// 개수 상한 초과 · 총 크기가 READ_HARD_LIMIT 초과 · 폴더뿐.
pub fn bundle_from_paths(paths: &[PathBuf]) -> Result<ClipContent, ClipError> {
    if paths.is_empty() {
        return Err(ClipError::Skipped("복사된 파일이 없습니다".into()));
    }
    if paths.len() > MAX_FILES_PER_CLIP {
        return Err(ClipError::Skipped(format!(
            "파일 {}개는 한 번에 보낼 수 있는 개수({MAX_FILES_PER_CLIP}개)를 넘습니다",
            paths.len()
        )));
    }

    // 1) 먼저 stat 만으로 크기·종류 판정 — 큰 파일을 읽지 않는다.
    let mut targets: Vec<&Path> = Vec::new();
    let mut total: u64 = 0;
    let mut had_dir = false;
    for p in paths {
        let Ok(md) = std::fs::metadata(p) else {
            tracing::warn!("파일 정보를 읽을 수 없어 건너뜁니다: {}", p.display());
            continue;
        };
        if md.is_dir() {
            had_dir = true;
            continue;
        }
        total = total.saturating_add(md.len());
        if total > READ_HARD_LIMIT {
            return Err(ClipError::Skipped(format!(
                "파일이 너무 큽니다 — {} (읽기 상한 {}). 파일 공유는 이 크기까지만 됩니다",
                mib(total),
                mib(READ_HARD_LIMIT)
            )));
        }
        targets.push(p.as_path());
    }
    if targets.is_empty() {
        return Err(ClipError::Skipped(if had_dir {
            "폴더는 아직 지원하지 않습니다 — 파일을 골라 복사해 주세요".into()
        } else {
            "복사된 파일을 읽을 수 없습니다".into()
        }));
    }

    // 2) 상한 안이므로 실제로 읽는다.
    let mut files = Vec::with_capacity(targets.len());
    for p in targets {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());
        match std::fs::read(p) {
            Ok(data) => files.push(FileEntry { name, data }),
            Err(e) => tracing::warn!("파일 읽기 실패({}): {e}", p.display()),
        }
    }
    if files.is_empty() {
        return Err(ClipError::Skipped("복사된 파일을 읽을 수 없습니다".into()));
    }

    let bundle = FileBundle::new(files);
    sb_proto::encode(&bundle)
        .map(ClipContent::files)
        .map_err(|e| ClipError::Access(format!("파일 번들 인코딩 실패: {e}")))
}

/// `Files` 콘텐츠 바이트 → 번들.
pub fn bundle_from_bytes(bytes: &[u8]) -> Result<FileBundle, ClipError> {
    sb_proto::decode(bytes).map_err(|e| ClipError::Access(format!("파일 번들 디코딩 실패: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip_reason(r: Result<ClipContent, ClipError>) -> String {
        match r {
            Err(ClipError::Skipped(m)) => m,
            other => panic!("Skipped 기대: {other:?}"),
        }
    }

    #[test]
    fn empty_and_unreadable_paths_report_a_reason() {
        assert!(skip_reason(bundle_from_paths(&[])).contains("없습니다"));
        let missing = bundle_from_paths(&[PathBuf::from("/definitely/not/here-xyz")]);
        assert!(!skip_reason(missing).is_empty());
    }

    #[test]
    fn packs_real_files_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("sb-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("보고서.bin");
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(&b, vec![7u8; 32]).unwrap();

        let clip = bundle_from_paths(&[a.clone(), b.clone()]).expect("번들");
        assert_eq!(clip.kind, sb_proto::ContentKind::Files);

        let bundle = bundle_from_bytes(&clip.bytes).unwrap();
        assert_eq!(bundle.files.len(), 2);
        assert_eq!(bundle.total_bytes(), 5 + 32);
        assert_eq!(bundle.files[0].name, "a.txt");
        assert_eq!(bundle.files[0].data, b"hello");
        assert_eq!(bundle.files[1].name, "보고서.bin");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directories_are_skipped_with_a_reason() {
        let dir = std::env::temp_dir().join(format!("sb-files-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let why = skip_reason(bundle_from_paths(std::slice::from_ref(&dir)));
        assert!(why.contains("폴더"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_files_are_not_even_read() {
        let dir = std::env::temp_dir().join(format!("sb-files-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.bin");
        // sparse 파일로 상한 초과 크기만 만든다(실제 디스크는 거의 쓰지 않음).
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(READ_HARD_LIMIT + 1).unwrap();
        drop(f);
        let why = skip_reason(bundle_from_paths(std::slice::from_ref(&big)));
        assert!(why.contains("너무 큽니다"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
