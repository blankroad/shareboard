//! # sb-clipboard
//!
//! OS 클립보드 접근 추상화 (PLAN.md §6).
//!
//! - [`ClipboardAccess`] — 읽기/쓰기/concealed 검사 트레이트. 코어는 이 트레이트에만 의존.
//! - [`ChangeWatcher`] — 변경 감지. v1 은 내용 지문 폴링(`PollingWatcher`); macOS `changeCount`
//!   최적화(D8)는 백엔드 교체로 얹는다.
//! - [`MockAccess`] — 테스트/headless 용.
//! - `ArboardAccess` (feature `arboard-backend`) — 실제 텍스트/PNG I/O.
//!
//! 변경 감지 이벤트가 와도 실제 read 전에 kind·크기로 1차 판별하는 원칙(§6, READ_HARD_LIMIT)은
//! 상위(sb-core 엔진)와 결합해 적용한다.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sb_proto::ContentKind;

#[cfg(feature = "arboard-backend")]
pub mod arboard_backend;

pub mod files;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(all(target_os = "linux", feature = "wayland-backend"))]
pub mod linux;

/// 시스템 기본 watcher — macOS 는 changeCount 기반(D8), 그 외는 내용 지문 폴링.
/// `arboard-backend` feature 필요(실제 OS I/O).
#[cfg(feature = "arboard-backend")]
pub fn system_watcher() -> Box<dyn ChangeWatcher + Send> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::ChangeCountWatcher::new(
            arboard_backend::ArboardAccess::new(),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(PollingWatcher::new(arboard_backend::ArboardAccess::new()))
    }
}

/// 클립보드 콘텐츠 한 건.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipContent {
    pub kind: ContentKind,
    /// 텍스트는 UTF-8, 이미지는 PNG 바이트.
    pub bytes: Vec<u8>,
}

impl ClipContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            kind: ContentKind::Text,
            bytes: s.into().into_bytes(),
        }
    }
    pub fn image_png(bytes: Vec<u8>) -> Self {
        Self {
            kind: ContentKind::ImagePng,
            bytes,
        }
    }
    /// `bytes` 는 [`sb_proto::FileBundle`] 인코딩(파일명 포함).
    pub fn files(bytes: Vec<u8>) -> Self {
        Self {
            kind: ContentKind::Files,
            bytes,
        }
    }
    /// 변경 감지용 지문(정확한 비교 대신 빠른 해시).
    pub fn fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        (self.kind as u8).hash(&mut h);
        self.bytes.hash(&mut h);
        h.finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("클립보드 접근 실패: {0}")]
    Access(String),
    #[error("지원하지 않는 포맷")]
    Unsupported,
}

/// OS 클립보드 접근.
pub trait ClipboardAccess {
    /// 현재 클립보드 콘텐츠(텍스트 우선, 없으면 이미지). 빈 클립보드면 `None`.
    fn read(&self) -> Result<Option<ClipContent>, ClipError>;
    /// 클립보드에 쓰기.
    fn write(&self, content: &ClipContent) -> Result<(), ClipError>;
    /// 클립보드에 **파일 경로 목록**을 올린다(붙여넣기 가능 상태). 파일은 호출자가 미리
    /// 디스크에 만들어 둬야 한다. 미지원 플랫폼/백엔드는 `Unsupported`.
    fn write_file_paths(&self, _paths: &[std::path::PathBuf]) -> Result<(), ClipError> {
        Err(ClipError::Unsupported)
    }
    /// concealed(비밀번호 매니저 힌트) 여부 — 감지 시 동기화 제외(§4.6). 미지원 백엔드는 false.
    fn is_concealed(&self) -> bool {
        false
    }
}

/// 변경 감지 + 쓰기 인터페이스. 앱은 이 트레이트 객체만 다룬다(백엔드 교체 가능).
pub trait ChangeWatcher {
    /// 마지막 관찰 이후 변경되었으면 새 콘텐츠, 아니면 None.
    fn poll(&mut self) -> Result<Option<ClipContent>, ClipError>;
    /// OS 클립보드에 쓰기 + 지문/카운터 갱신(에코 재감지 억제).
    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError>;
    /// 파일 경로 목록을 클립보드에 올리고 에코 재감지를 억제한다.
    fn write_file_paths(&mut self, paths: &[std::path::PathBuf]) -> Result<(), ClipError>;
    /// 다음 폴에서 이 콘텐츠를 "이미 본 것"으로 취급.
    fn note_written(&mut self, content: &ClipContent);
    /// 현재 클립보드가 concealed(비밀번호 매니저 힌트)인가.
    fn is_concealed(&self) -> bool {
        false
    }
}

/// 내용 지문 폴링 watcher (§6 D8 의 이식 가능한 baseline).
///
/// macOS 는 `changeCount` 정수 비교가 더 싸지만, 이 구현은 전 플랫폼에서 동작하며
/// 원격 write 시 [`Self::note_written`] 으로 지문을 갱신해 에코 재감지를 억제한다.
pub struct PollingWatcher<A: ClipboardAccess> {
    access: A,
    last: Option<u64>,
}

impl<A: ClipboardAccess> PollingWatcher<A> {
    pub fn new(access: A) -> Self {
        Self { access, last: None }
    }

    pub fn access(&self) -> &A {
        &self.access
    }
}

impl<A: ClipboardAccess> ChangeWatcher for PollingWatcher<A> {
    fn poll(&mut self) -> Result<Option<ClipContent>, ClipError> {
        match self.access.read()? {
            Some(c) => {
                let fp = c.fingerprint();
                if self.last != Some(fp) {
                    self.last = Some(fp);
                    Ok(Some(c))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    fn write(&mut self, content: &ClipContent) -> Result<(), ClipError> {
        self.access.write(content)?;
        self.last = Some(content.fingerprint());
        Ok(())
    }

    fn write_file_paths(&mut self, paths: &[std::path::PathBuf]) -> Result<(), ClipError> {
        self.access.write_file_paths(paths)?;
        // 우리가 만든 파일명이 원본과 다를 수 있으므로(중복 회피 접미사) 실제로 올라간 내용을
        // 다시 읽어 지문을 맞춘다 — 그래야 다음 폴에서 새 클립으로 오인하지 않는다.
        self.last = self.access.read().ok().flatten().map(|c| c.fingerprint());
        Ok(())
    }

    fn note_written(&mut self, content: &ClipContent) {
        self.last = Some(content.fingerprint());
    }

    fn is_concealed(&self) -> bool {
        self.access.is_concealed()
    }
}

/// 테스트/headless 용 mock 클립보드. `RefCell` 사용(단일 스레드).
#[derive(Default)]
pub struct MockAccess {
    current: RefCell<Option<ClipContent>>,
    concealed: RefCell<bool>,
    writes: RefCell<Vec<ClipContent>>,
}

impl MockAccess {
    pub fn new() -> Self {
        Self::default()
    }
    /// 외부(사용자/타 앱)가 클립보드를 바꾼 상황 모사.
    pub fn set_external(&self, content: ClipContent) {
        *self.current.borrow_mut() = Some(content);
    }
    pub fn set_concealed(&self, v: bool) {
        *self.concealed.borrow_mut() = v;
    }
    /// 우리가 쓴 콘텐츠 기록(에코 테스트용).
    pub fn writes(&self) -> Vec<ClipContent> {
        self.writes.borrow().clone()
    }
}

impl ClipboardAccess for MockAccess {
    fn read(&self) -> Result<Option<ClipContent>, ClipError> {
        Ok(self.current.borrow().clone())
    }
    fn write(&self, content: &ClipContent) -> Result<(), ClipError> {
        *self.current.borrow_mut() = Some(content.clone());
        self.writes.borrow_mut().push(content.clone());
        Ok(())
    }
    fn is_concealed(&self) -> bool {
        *self.concealed.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_detects_only_changes() {
        let mock = MockAccess::new();
        mock.set_external(ClipContent::text("a"));
        let mut w = PollingWatcher::new(mock);

        assert_eq!(w.poll().unwrap(), Some(ClipContent::text("a")));
        assert_eq!(w.poll().unwrap(), None, "미변경은 None");

        w.access().set_external(ClipContent::text("b"));
        assert_eq!(w.poll().unwrap(), Some(ClipContent::text("b")));
    }

    #[test]
    fn write_suppresses_echo_redetect() {
        let mut w = PollingWatcher::new(MockAccess::new());
        // 원격 콘텐츠를 씀 → 다음 poll 에서 재감지 안 됨(에코 억제 보조).
        w.write(&ClipContent::text("from-peer")).unwrap();
        assert_eq!(w.poll().unwrap(), None, "우리가 쓴 건 변경으로 재감지 안 함");
    }

    #[test]
    fn empty_clipboard_is_none() {
        let mut w = PollingWatcher::new(MockAccess::new());
        assert_eq!(w.poll().unwrap(), None);
    }

    #[test]
    fn concealed_flag_propagates() {
        let mock = MockAccess::new();
        mock.set_concealed(true);
        assert!(mock.is_concealed());
    }
}

#[cfg(test)]
mod engine_integration {
    //! mock 클립보드 → 엔진 발행까지 실제 결선.
    use super::*;
    use sb_core::{EngineConfig, LocalOutcome, SyncEngine};
    use sb_crypto::GroupKey;

    #[test]
    fn clipboard_change_drives_engine_emit() {
        let mock = MockAccess::new();
        mock.set_external(ClipContent::text("copied text"));
        let mut watcher = PollingWatcher::new(mock);

        let gk = GroupKey::from_bytes(0, [5u8; 32]);
        let cfg = EngineConfig {
            enabled: true,
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_content_bytes: 10 * 1024 * 1024,
            history_cap: 30,
        };
        let mut engine = SyncEngine::new([1u8; 32], gk, cfg);

        // 워처가 변경 감지 → 엔진이 신호 발행.
        let change = watcher.poll().unwrap().expect("변경 감지");
        let out = engine
            .on_local_clipboard(change.kind, &change.bytes, 1000)
            .unwrap();
        assert!(
            matches!(out, LocalOutcome::Emit(_)),
            "클립보드 변경이 신호 발행으로 이어짐"
        );

        // 같은 내용 재폴 → 변경 없음 → 발행 없음.
        assert_eq!(watcher.poll().unwrap(), None);
    }
}
