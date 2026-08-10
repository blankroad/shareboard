//! Windows 파일 클립보드 — `CF_HDROP`(탐색기 복사/붙여넣기) 읽기·쓰기.
//!
//! `clipboard-win` 은 arboard 가 이미 쓰는 크레이트다(의존성 추가 없음). 쓰기는 반드시
//! 클립보드를 비운 뒤 넣는다 — 비우지 않으면 이전 텍스트가 남아 우리 읽기 경로(텍스트 우선)가
//! 그 텍스트를 새 클립으로 오인한다.

use std::path::PathBuf;

use clipboard_win::{formats, options, raw, Clipboard, Getter};

use crate::ClipError;

fn open() -> Result<Clipboard, ClipError> {
    // 다른 앱이 클립보드를 잡고 있을 수 있어 재시도한다.
    Clipboard::new_attempts(10).map_err(|e| ClipError::Access(format!("클립보드 열기 실패: {e}")))
}

/// 클립보드의 CF_HDROP 파일 목록. 파일 클립이 아니면 빈 Vec.
pub fn read_file_paths() -> Vec<PathBuf> {
    let Ok(_clip) = open() else {
        return Vec::new();
    };
    if !raw::is_format_avail(u32::from(&formats::FileList)) {
        return Vec::new();
    }
    let mut out: Vec<PathBuf> = Vec::new();
    if let Err(e) = formats::FileList.read_clipboard(&mut out) {
        tracing::warn!("CF_HDROP 읽기 실패: {e}");
        return Vec::new();
    }
    out
}

/// 경로들을 CF_HDROP 으로 올린다(탐색기 붙여넣기 가능).
pub fn write_file_paths(paths: &[PathBuf]) -> Result<(), ClipError> {
    let _clip = open()?;
    let strs: Vec<String> = paths
        .iter()
        .filter_map(|p| p.to_str().map(String::from))
        .collect();
    if strs.is_empty() {
        return Err(ClipError::Unsupported);
    }
    // DoClear = 기존 포맷(텍스트 등)을 먼저 비운다.
    raw::set_file_list_with(&strs, options::DoClear)
        .map_err(|e| ClipError::Access(format!("CF_HDROP 쓰기 실패: {e}")))
}
