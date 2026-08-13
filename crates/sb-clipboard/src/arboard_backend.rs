//! arboard 기반 실제 OS 클립보드 백엔드 (§6). 텍스트 + PNG(RGBA↔PNG 변환).
//!
//! 주의: concealed(비밀번호 매니저) 힌트 감지는 arboard 가 노출하지 않아 현재 `false`.
//! 플랫폼별 pasteboard 타입 검사(macOS `org.nspasteboard.ConcealedType` 등)는 후속 과제(§4.6 D17).
//! macOS `changeCount` 기반 저비용 감지(D8)도 후속 — 현재는 상위 `PollingWatcher` 사용.

use std::io::Cursor;

use arboard::{Clipboard, ImageData};
use image::{DynamicImage, ImageFormat, RgbaImage};

use crate::{ClipContent, ClipError, ClipboardAccess};

/// arboard 백엔드.
///
/// 핸들 수명은 플랫폼마다 다르게 잡는다 — [`with_clipboard`] 주석 참고.
pub struct ArboardAccess;

impl ArboardAccess {
    pub fn new() -> Self {
        ArboardAccess
    }
}

impl Default for ArboardAccess {
    fn default() -> Self {
        Self::new()
    }
}

fn err<E: std::fmt::Display>(e: E) -> ClipError {
    ClipError::Access(e.to_string())
}

/// 클립보드 핸들을 열어 `f` 를 실행한다.
///
/// **Linux 는 클립보드 내용이 소유 프로세스 안에 산다**(X11 selection / Wayland data source).
/// 그래서 `Clipboard` 를 쓰기마다 만들고 버리면 함수가 끝나는 순간 선택 소유권이 사라져
/// 다른 앱에서 붙여넣기가 되지 않는다 — "히스토리에는 들어왔는데 Ctrl+V 는 옛 내용"이 된다.
/// (Docker 검증: 핸들을 살려 두면 `xclip -o -t UTF8_STRING` 로 읽히고, drop 하면 소유자 자체가
/// 사라진다.) 따라서 Linux 에서는 프로세스 수명 동안 인스턴스 하나를 유지하고 읽기·쓰기를
/// 그 하나로 직렬화한다.
///
/// macOS/Windows 는 OS(NSPasteboard / Win32 클립보드)가 데이터를 보관하므로 호출마다 열어도
/// 되고, 기존 동작을 그대로 둔다.
#[cfg(target_os = "linux")]
fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> R) -> Result<R, ClipError> {
    use std::sync::{Mutex, OnceLock};

    static SHARED: OnceLock<Mutex<Clipboard>> = OnceLock::new();

    if SHARED.get().is_none() {
        // OnceLock::get_or_init 은 실패를 표현할 수 없어 먼저 만들어 넣는다(경합해도 안전).
        let cb = Clipboard::new().map_err(err)?;
        let _ = SHARED.set(Mutex::new(cb));
    }
    let cell = SHARED
        .get()
        .ok_or_else(|| ClipError::Access("클립보드 초기화 실패".into()))?;
    let mut guard = cell
        .lock()
        .map_err(|_| ClipError::Access("클립보드 잠금 오염".into()))?;
    Ok(f(&mut guard))
}

#[cfg(not(target_os = "linux"))]
fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> R) -> Result<R, ClipError> {
    let mut cb = Clipboard::new().map_err(err)?;
    Ok(f(&mut cb))
}

fn rgba_to_png(img: ImageData) -> Result<Vec<u8>, ClipError> {
    let w = img.width as u32;
    let h = img.height as u32;
    let raw = img.bytes.into_owned();
    let rgba = RgbaImage::from_raw(w, h, raw).ok_or(ClipError::Unsupported)?;
    let mut buf = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(err)?;
    Ok(buf)
}

fn png_to_rgba(png: &[u8]) -> Result<ImageData<'static>, ClipError> {
    let img = image::load_from_memory_with_format(png, ImageFormat::Png).map_err(err)?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(ImageData {
        width: w as usize,
        height: h as usize,
        bytes: rgba.into_raw().into(),
    })
}

impl ClipboardAccess for ArboardAccess {
    fn read(&self) -> Result<Option<ClipContent>, ClipError> {
        // 파일 먼저 판정한다 — 파일을 복사하면 파일명이 텍스트로도 올라오는 플랫폼이 있어,
        // 텍스트를 먼저 보면 "파일명 문자열"을 동기화해 버린다.
        let paths = crate::files::clipboard_file_paths();
        if !paths.is_empty() {
            // 상한 초과·폴더 등은 Skipped 로 올려 사용자에게 이유를 보여준다.
            // (텍스트로 대체하지 않는다 — 파일명 문자열이 동기화되면 더 헷갈린다.)
            return crate::files::bundle_from_paths(&paths).map(Some);
        }

        // 텍스트 우선(§5.5 다중 포맷 우선순위: 텍스트 > 이미지).
        if let Some(t) = with_clipboard(|cb| cb.get_text().ok())? {
            if !t.is_empty() {
                return Ok(Some(ClipContent::text(t)));
            }
        }
        if let Some(img) = with_clipboard(|cb| cb.get_image().ok())? {
            return Ok(Some(ClipContent::image_png(rgba_to_png(img)?)));
        }

        // 마지막 폴백: 앱에서 복사한 데이터(Preview 의 PDF, 문서 조각 등)를 파일로 만들어 보낸다.
        // 이게 없으면 "클립보드엔 있는데 shareboard 엔 안 보인다"가 된다.
        #[cfg(target_os = "macos")]
        if let Some((ext, data)) = crate::macos::read_data_as_file() {
            return crate::files::bundle_from_data(&format!("clipboard.{ext}"), data).map(Some);
        }

        Ok(None)
    }

    fn write(&self, content: &ClipContent) -> Result<(), ClipError> {
        match content.kind {
            sb_proto::ContentKind::Text => {
                let s = String::from_utf8_lossy(&content.bytes).into_owned();
                with_clipboard(|cb| cb.set_text(s).map_err(err))?
            }
            sb_proto::ContentKind::ImagePng => {
                let data = png_to_rgba(&content.bytes)?;
                with_clipboard(|cb| cb.set_image(data).map_err(err))?
            }
            // 파일은 디스크에 실체가 있어야 붙여넣기가 성립한다 → 앱이 파일을 만든 뒤
            // write_file_paths 로 경로를 올린다(§파일 클립보드).
            sb_proto::ContentKind::Files => Err(ClipError::Unsupported),
        }
    }

    fn write_file_paths(&self, paths: &[std::path::PathBuf]) -> Result<(), ClipError> {
        crate::files::set_clipboard_file_paths(paths)
    }
}
