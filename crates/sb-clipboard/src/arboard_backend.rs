//! arboard 기반 실제 OS 클립보드 백엔드 (§6). 텍스트 + PNG(RGBA↔PNG 변환).
//!
//! 주의: concealed(비밀번호 매니저) 힌트 감지는 arboard 가 노출하지 않아 현재 `false`.
//! 플랫폼별 pasteboard 타입 검사(macOS `org.nspasteboard.ConcealedType` 등)는 후속 과제(§4.6 D17).
//! macOS `changeCount` 기반 저비용 감지(D8)도 후속 — 현재는 상위 `PollingWatcher` 사용.

use std::io::Cursor;

use arboard::{Clipboard, ImageData};
use image::{DynamicImage, ImageFormat, RgbaImage};

use crate::{ClipContent, ClipError, ClipboardAccess};

/// arboard 백엔드. 상태를 들지 않고 호출마다 클립보드 핸들을 연다(단순·안전).
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

        let mut cb = Clipboard::new().map_err(err)?;
        // 텍스트 우선(§5.5 다중 포맷 우선순위: 텍스트 > 이미지).
        if let Ok(t) = cb.get_text() {
            if !t.is_empty() {
                return Ok(Some(ClipContent::text(t)));
            }
        }
        if let Ok(img) = cb.get_image() {
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
                let mut cb = Clipboard::new().map_err(err)?;
                let s = String::from_utf8_lossy(&content.bytes).into_owned();
                cb.set_text(s).map_err(err)
            }
            sb_proto::ContentKind::ImagePng => {
                let mut cb = Clipboard::new().map_err(err)?;
                let data = png_to_rgba(&content.bytes)?;
                cb.set_image(data).map_err(err)
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
