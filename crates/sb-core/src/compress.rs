//! 콘텐츠 압축 (§4.6, §5.6). zstd level 3, **암호화 전** 적용. 4KiB 초과 텍스트만.

use sb_proto::params::ZSTD_THRESHOLD;
use sb_proto::ContentKind;

const ZSTD_LEVEL: i32 = 3;

/// 필요 시 압축. 반환 `(bytes, compressed)`. 압축이 이득 없으면 원문 유지.
pub fn maybe_compress(kind: ContentKind, plaintext: &[u8]) -> (Vec<u8>, bool) {
    if kind == ContentKind::Text && plaintext.len() > ZSTD_THRESHOLD {
        if let Ok(z) = zstd::stream::encode_all(plaintext, ZSTD_LEVEL) {
            if z.len() < plaintext.len() {
                return (z, true);
            }
        }
    }
    (plaintext.to_vec(), false)
}

/// `maybe_compress` 의 역.
pub fn maybe_decompress(bytes: &[u8], compressed: bool) -> Result<Vec<u8>, crate::CoreError> {
    if compressed {
        zstd::stream::decode_all(bytes).map_err(|_| crate::CoreError::Decompress)
    } else {
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_text_not_compressed() {
        let (b, c) = maybe_compress(ContentKind::Text, b"short");
        assert!(!c);
        assert_eq!(b, b"short");
    }

    #[test]
    fn large_repetitive_text_compresses_and_roundtrips() {
        let big = "shareboard ".repeat(1000);
        let (b, c) = maybe_compress(ContentKind::Text, big.as_bytes());
        assert!(c, "반복 텍스트는 압축됨");
        assert!(b.len() < big.len());
        let back = maybe_decompress(&b, c).unwrap();
        assert_eq!(back, big.as_bytes());
    }

    #[test]
    fn images_never_compressed() {
        let data = vec![0xABu8; 10_000];
        let (_b, c) = maybe_compress(ContentKind::ImagePng, &data);
        assert!(!c, "PNG 는 이미 압축 — 이중 압축 안 함");
    }
}
