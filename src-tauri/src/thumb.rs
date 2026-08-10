//! 히스토리 이미지 썸네일 — PNG 본문을 작게 줄여 data URL 로 만든다.
//!
//! 본문은 메모리(body_cache)에만 있으므로 썸네일도 앱 안에서 생성한다(외부 요청 0).
//! 결과는 `Core::thumb_cache` 에 캐시해 목록 리렌더마다 디코딩하지 않는다.

use base64::Engine;
use image::ImageFormat;

use crate::core::ThumbView;

/// 썸네일 최대 변 길이(px). UI 표시(48px)의 2배 = HiDPI 대응.
const MAX_EDGE: u32 = 96;

/// PNG 바이트 → 축소 PNG data URL. 디코딩 실패(손상·미지원)면 None.
///
/// CPU 를 쓰므로 커맨드에서 `spawn_blocking` 으로 호출한다.
pub fn render(png: &[u8]) -> Option<ThumbView> {
    let img = image::load_from_memory_with_format(png, ImageFormat::Png).ok()?;
    let (width, height) = (img.width(), img.height());
    // thumbnail: 비율 유지 + 빠른 축소. 원본이 더 작으면 그대로 둔다.
    let small = if width > MAX_EDGE || height > MAX_EDGE {
        img.thumbnail(MAX_EDGE, MAX_EDGE)
    } else {
        img
    };

    let mut out = Vec::new();
    small
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .ok()?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&out);
    Some(ThumbView {
        data_url: format!("data:image/png;base64,{b64}"),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 120, 220, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn reports_original_size_and_shrinks() {
        let t = render(&png_of(400, 200)).expect("썸네일");
        assert_eq!((t.width, t.height), (400, 200));
        assert!(t.data_url.starts_with("data:image/png;base64,"));
        // 원본보다 확실히 작아야 한다.
        assert!(t.data_url.len() < png_of(400, 200).len() * 2);
    }

    #[test]
    fn keeps_small_images_as_is() {
        let t = render(&png_of(32, 16)).expect("썸네일");
        assert_eq!((t.width, t.height), (32, 16));
    }

    #[test]
    fn rejects_non_png() {
        assert!(render(b"not a png").is_none());
    }
}
