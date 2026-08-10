//! 파일 클립보드 페이로드 (`ContentKind::Files`).
//!
//! OS 클립보드에서 복사한 파일 목록을 **하나의 콘텐츠 바이트열**로 담는다. 기존 파이프라인
//! (GK 봉인 → 청크 릴레이 → LWW → 히스토리)이 텍스트·이미지와 똑같이 처리하도록 하기 위한
//! 설계다. 파일 **이름도 암호문 안**에 있으므로 서버는 이름·개수·크기 분포를 알 수 없다.
//!
//! 파일명은 **받는 쪽에서 반드시** [`safe_file_name`] 으로 정규화해야 한다 — 보낸 쪽 이름을
//! 그대로 경로에 쓰면 `../` 탈출이나 Windows 예약 이름에 노출된다.

use serde::{Deserialize, Serialize};

/// 파일 한 개(이름 + 내용).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// 원본 파일명(경로 없음). 신뢰하지 말 것 — 쓸 때 `safe_file_name` 통과 필수.
    pub name: String,
    pub data: Vec<u8>,
}

/// 클립보드 파일 묶음.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBundle {
    pub files: Vec<FileEntry>,
}

impl FileBundle {
    pub fn new(files: Vec<FileEntry>) -> Self {
        Self { files }
    }

    /// 전체 파일 크기 합(이름·인코딩 오버헤드 제외).
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.data.len() as u64).sum()
    }

    /// UI 미리보기 — "a.txt, b.png (2개)". 이름은 평문이지만 앱 안에서만 쓰인다.
    pub fn preview(&self) -> String {
        let names: Vec<&str> = self.files.iter().take(3).map(|f| f.name.as_str()).collect();
        let mut s = names.join(", ");
        if self.files.len() > names.len() {
            s.push_str(" …");
        }
        format!("{s} ({}개)", self.files.len())
    }
}

/// 받은 파일명을 로컬에 쓰기 안전한 형태로 정규화한다.
///
/// - 경로 성분 제거(`/`, `\`, 드라이브 문자) → 파일명만 남긴다
/// - `.` / `..` / 빈 이름 → `unnamed`
/// - 제어문자와 Windows 금지문자(`<>:"|?*`) → `_`
/// - Windows 예약 이름(CON, PRN, NUL, COM1 …) → 접미사 `_`
/// - 끝의 공백·점 제거(Windows 는 이런 이름을 만들 수 없다)
/// - 바이트 길이 200 으로 절단(확장자 최대한 보존)
pub fn safe_file_name(raw: &str) -> String {
    // 경로 성분 제거 — 두 구분자 모두 자른다(윈도우에서 온 이름이 유닉스에 쓰일 수 있다).
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim_start_matches(char::is_whitespace);

    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();

    // 끝의 점·공백 제거.
    while cleaned.ends_with('.') || cleaned.ends_with(' ') {
        cleaned.pop();
    }

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "unnamed".into();
    }

    // Windows 예약 이름(확장자 유무 무관).
    let stem_upper = cleaned.split('.').next().unwrap_or(&cleaned).to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem_upper.as_str()) {
        cleaned.push('_');
    }

    truncate_keeping_extension(&cleaned, 200)
}

/// 바이트 길이 상한을 지키며 확장자를 최대한 보존(문자 경계 안전).
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    // 확장자는 16바이트 이내일 때만 보존 시도.
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| e.len() <= 16 && !e.is_empty());
    let keep = match ext {
        Some(e) => max.saturating_sub(e.len() + 1),
        None => max,
    };
    let mut stem: String = String::new();
    for ch in name.chars() {
        if stem.len() + ch.len_utf8() > keep {
            break;
        }
        stem.push(ch);
    }
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_traversal() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("..\\..\\windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(safe_file_name("/absolute/path/report.pdf"), "report.pdf");
        assert_eq!(safe_file_name(".."), "unnamed");
        assert_eq!(safe_file_name(""), "unnamed");
    }

    #[test]
    fn neutralizes_forbidden_and_control_chars() {
        assert_eq!(safe_file_name("a:b?c*.txt"), "a_b_c_.txt");
        assert_eq!(safe_file_name("tab\there.txt"), "tab_here.txt");
        // 끝의 점·공백은 Windows 에서 만들 수 없다.
        assert_eq!(safe_file_name("weird.   "), "weird");
    }

    #[test]
    fn escapes_windows_reserved_names() {
        assert_eq!(safe_file_name("CON"), "CON_");
        assert_eq!(safe_file_name("nul.txt"), "nul.txt_");
        assert_eq!(safe_file_name("com9"), "com9_");
        assert_eq!(safe_file_name("console.log"), "console.log");
    }

    #[test]
    fn truncates_long_names_keeping_extension() {
        let long = format!("{}.png", "a".repeat(500));
        let out = safe_file_name(&long);
        assert!(out.len() <= 200, "{}", out.len());
        assert!(out.ends_with(".png"));
    }

    #[test]
    fn keeps_unicode_names_intact() {
        assert_eq!(safe_file_name("보고서 최종.xlsx"), "보고서 최종.xlsx");
        // 한글이 잘려도 문자 경계는 깨지지 않는다.
        let long = format!("{}.txt", "가".repeat(300));
        let out = safe_file_name(&long);
        assert!(out.len() <= 200);
        assert!(out.ends_with(".txt"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn bundle_preview_lists_names_and_count() {
        let b = FileBundle::new(vec![
            FileEntry {
                name: "a.txt".into(),
                data: vec![1, 2, 3],
            },
            FileEntry {
                name: "b.png".into(),
                data: vec![4],
            },
        ]);
        assert_eq!(b.total_bytes(), 4);
        assert_eq!(b.preview(), "a.txt, b.png (2개)");
    }

    #[test]
    fn bundle_roundtrips_through_cbor() {
        let b = FileBundle::new(vec![FileEntry {
            name: "보고서.pdf".into(),
            data: vec![9; 100],
        }]);
        let bytes = crate::encode(&b).unwrap();
        let back: FileBundle = crate::decode(&bytes).unwrap();
        assert_eq!(back, b);
    }
}
