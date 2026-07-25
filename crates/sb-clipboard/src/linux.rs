//! Linux/Wayland 네이티브 클립보드 백엔드 (§6) — **실험적, Linux 전용**.
//!
//! ## 배경 (PLAN.md §6, D7)
//! - X11 세션에서는 상위 `arboard` 백엔드가 이미 동작한다(XFixes 이벤트).
//! - 순수 Wayland 에서는 `wlr-data-control-v1`/`ext-data-control-v1` 프로토콜이 필요하다.
//!   KWin·wlroots·Mutter(GNOME ≥49)는 지원, **GNOME ≤48 은 미지원 → XWayland+arboard 폴백**.
//!
//! ## 현재 범위 (스캐폴드)
//! - [`WaylandAccess`]: `wl-clipboard-rs` 로 텍스트/PNG read·write (data-control I/O).
//! - 변경 **감시**는 아직 `PollingWatcher<WaylandAccess>` 로 대체한다. 네이티브 이벤트 감시
//!   (data-control 세션의 `selection` 이벤트 구독)는 후속 과제 — `wl-clipboard-rs` 는 watcher 를
//!   노출하지 않으므로 `wayland-client` + `wayland-protocols` 로 직접 구현해야 한다(§6 D7).
//!
//! 이 모듈은 `wayland-backend` feature + `target_os = "linux"` 에서만 컴파일된다.
//! macOS/Windows 빌드와 기본 CI 에는 영향이 없으며, Linux 실기기/CI 검증이 필요하다.

use std::io::Read;

use wl_clipboard_rs::copy::{MimeType as CopyMime, Options, Source};
use wl_clipboard_rs::paste::{get_contents, ClipboardType, MimeType as PasteMime, Seat};

use sb_proto::ContentKind;

use crate::{ClipContent, ClipError, ClipboardAccess};

const TEXT_MIME: &str = "text/plain;charset=utf-8";
const IMAGE_MIME: &str = "image/png";

/// Wayland data-control 기반 클립보드 접근.
pub struct WaylandAccess;

impl WaylandAccess {
    pub fn new() -> Self {
        WaylandAccess
    }
}

impl Default for WaylandAccess {
    fn default() -> Self {
        Self::new()
    }
}

fn read_mime(mime: &str) -> Option<Vec<u8>> {
    match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMime::Specific(mime),
    ) {
        Ok((mut pipe, _mime)) => {
            let mut buf = Vec::new();
            pipe.read_to_end(&mut buf).ok()?;
            if buf.is_empty() {
                None
            } else {
                Some(buf)
            }
        }
        Err(_) => None,
    }
}

impl ClipboardAccess for WaylandAccess {
    fn read(&self) -> Result<Option<ClipContent>, ClipError> {
        // 다중 포맷 우선순위: 텍스트 > 이미지 (§5.5).
        if let Some(bytes) = read_mime(TEXT_MIME) {
            return Ok(Some(ClipContent {
                kind: ContentKind::Text,
                bytes,
            }));
        }
        if let Some(bytes) = read_mime(IMAGE_MIME) {
            return Ok(Some(ClipContent {
                kind: ContentKind::ImagePng,
                bytes,
            }));
        }
        Ok(None)
    }

    fn write(&self, content: &ClipContent) -> Result<(), ClipError> {
        let mime = match content.kind {
            ContentKind::Text => CopyMime::Specific(TEXT_MIME.to_string()),
            ContentKind::ImagePng => CopyMime::Specific(IMAGE_MIME.to_string()),
        };
        Options::new()
            .copy(Source::Bytes(content.bytes.clone().into()), mime)
            .map_err(|e| ClipError::Access(e.to_string()))
    }

    // concealed: Wayland 에는 표준 concealed MIME 관례가 약함. KDE `x-kde-passwordManagerHint`
    // 등을 read 시 검사하는 로직은 후속(§4.6 D17). 현재 false.
}
