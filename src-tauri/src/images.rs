//! 이미지 저장 폴더 — 히스토리 이미지를 실제 PNG 파일로 꺼내 둔다.
//!
//! 이미지 본문은 메모리에만 있어서 화면의 썸네일이 `data:` URL 이었다. 그래서 WebView 의
//! "이미지 링크 복사"는 수만 자짜리 base64 문자열을 줬고, 밖으로 끌어내면 그 문자열이 파일명이
//! 되어 Finder/탐색기가 받지 못했다. 저장 폴더가 지정되어 있으면 이미지를 짧은 이름의 파일로
//! 써 두고, 경로 복사·드래그는 그 파일을 쓴다. 폴더가 없으면 아무것도 쓰지 않는다(메모리 전용).
//!
//! 쌓이지 않도록 보관 기간이 지난 파일은 [`cleanup`] 이 지운다 — shareboard 가 만든 이름의
//! 파일만 건드리므로 사용자가 같은 폴더에 둔 다른 파일은 안전하다.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 저장 파일명 — `shareboard-20261029-153012.png` (로컬 시각). 같은 초에 겹치면
/// [`crate::received::write_unique`] 가 ` (2)` 를 붙인다.
pub fn file_name(created_at_ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(created_at_ms as i64).single() {
        Some(t) => format!("shareboard-{}.png", t.format("%Y%m%d-%H%M%S")),
        None => format!("shareboard-{created_at_ms}.png"),
    }
}

/// PNG 바이트를 `dir` 에 저장하고 경로를 돌려준다(폴더가 없으면 만든다).
/// 같은 이름·같은 내용이 이미 있으면 새로 쓰지 않고 그 파일을 쓴다 — 이때 수정 시각을 지금으로
/// 갱신해, 방금 다시 쓴 이미지가 보관 기간 정리에 걸려 사라지지 않게 한다.
pub fn save(dir: &Path, created_at_ms: u64, png: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let (path, created) = crate::received::write_unique(dir, &file_name(created_at_ms), png)?;
    if !created {
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }
    Ok(path)
}

/// shareboard 가 만든 파일 이름인가 — `shareboard-….png`, 충돌 접미사 ` (n)` 포함.
/// 정리 대상은 이것뿐이다(사용자 파일 보호).
pub fn is_ours(name: &str) -> bool {
    name.starts_with("shareboard-") && name.ends_with(".png") && !name.starts_with('.')
}

/// `dir` 에서 `keep_days` 보다 오래된(수정 시각 기준) shareboard 이미지를 지운다.
/// `keep` 에 든 경로(핀 고정 항목)는 남긴다. `keep_days == 0` 이면 아무것도 지우지 않는다.
/// 지운 개수를 돌려준다. 개별 실패는 건너뛴다(다음 정리에서 다시 시도).
pub fn cleanup(dir: &Path, keep_days: u32, keep: &HashSet<PathBuf>, now: SystemTime) -> usize {
    if keep_days == 0 {
        return 0;
    }
    let Some(cutoff) = now.checked_sub(Duration::from_secs(u64::from(keep_days) * 86_400)) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let name = e.file_name();
        if !is_ours(&name.to_string_lossy()) {
            continue;
        }
        let path = e.path();
        if keep.contains(&path) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let old = meta.modified().map(|m| m < cutoff).unwrap_or(false);
        if old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 설정 화면에서 받은 폴더 경로 검증 — 절대 경로만 받고, 없으면 만든다.
pub fn prepare_dir(raw: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(raw.trim());
    if p.as_os_str().is_empty() || !p.is_absolute() {
        return Err(format!("절대 경로가 아닙니다: {raw}"));
    }
    std::fs::create_dir_all(&p).map_err(|e| format!("폴더를 만들 수 없습니다 ({}): {e}", p.display()))?;
    if !p.is_dir() {
        return Err(format!("폴더가 아닙니다: {}", p.display()));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sb-img-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        d
    }

    #[test]
    fn name_is_short_and_safe() {
        let n = file_name(1_761_700_000_000);
        assert!(n.starts_with("shareboard-") && n.ends_with(".png"), "{n}");
        assert!(n.len() < 40, "파일명은 짧아야 한다: {n}");
        assert!(!n.contains('/') && !n.contains(':'), "{n}");
    }

    #[test]
    fn saves_and_reuses_same_content() {
        let d = tmp_dir("save");
        let a = save(&d, 1_000, b"png-a").unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"png-a");
        let again = save(&d, 1_000, b"png-a").unwrap();
        assert_eq!(a, again, "같은 이미지는 파일을 하나만 만든다");
        let other = save(&d, 1_000, b"png-b").unwrap();
        assert_ne!(a, other, "같은 초의 다른 이미지는 덮어쓰지 않는다");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn recognizes_only_our_files() {
        assert!(is_ours("shareboard-20261029-153012.png"));
        assert!(is_ours("shareboard-20261029-153012 (2).png"));
        assert!(!is_ours("vacation.png"));
        assert!(!is_ours("shareboard-notes.txt"));
        assert!(!is_ours(".shareboard-20261029-153012.png.part"));
    }

    #[test]
    fn cleanup_removes_only_old_unpinned_own_files() {
        let d = tmp_dir("clean");
        std::fs::create_dir_all(&d).unwrap();
        let old = d.join("shareboard-20200101-000000.png");
        let pinned = d.join("shareboard-20200101-000001.png");
        let fresh = d.join("shareboard-20990101-000000.png");
        let users = d.join("vacation.png");
        for p in [&old, &pinned, &fresh, &users] {
            std::fs::write(p, b"x").unwrap();
        }
        // 파일은 방금 만들어졌으니 "지금"을 10일 뒤로 잡아 old/pinned/users 를 오래된 것으로 만든다.
        // fresh 는 그 시점 기준으로도 새로워야 하므로 수정 시각을 미래로 민다.
        let now = SystemTime::now() + Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options().write(true).open(&fresh).unwrap();
        f.set_modified(now).unwrap();
        drop(f);

        let keep: HashSet<PathBuf> = [pinned.clone()].into_iter().collect();
        assert_eq!(cleanup(&d, 7, &keep, now), 1);
        assert!(!old.exists(), "기간 지난 shareboard 파일은 삭제");
        assert!(pinned.exists(), "핀 고정 항목은 보존");
        assert!(fresh.exists(), "기간 안의 파일은 보존");
        assert!(users.exists(), "사용자 파일은 절대 건드리지 않는다");

        assert_eq!(cleanup(&d, 0, &HashSet::new(), now), 0, "0 = 계속 보관");
        assert!(pinned.exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rejects_relative_dir() {
        assert!(prepare_dir("pics").is_err());
        assert!(prepare_dir("  ").is_err());
        let d = tmp_dir("prep");
        assert_eq!(prepare_dir(d.to_str().unwrap()).unwrap(), d);
        assert!(d.is_dir());
        std::fs::remove_dir_all(&d).ok();
    }
}
