//! 클립보드 상태 확인 도구 — 플랫폼별 파일 클립보드 지원을 실기기에서 검증할 때 쓴다.
//!
//! ```bash
//! cargo run -p sb-clipboard --example clip_probe                 # 현재 클립보드 내용 출력
//! cargo run -p sb-clipboard --example clip_probe -- <파일> [...]  # 파일 경로를 클립보드에 올림
//! ```
//!
//! macOS 에서 파일 클립을 만들려면:
//! `osascript -e 'set the clipboard to (POSIX file "/tmp/a.txt")'`

use std::path::PathBuf;

use sb_clipboard::{arboard_backend::ArboardAccess, files, ClipboardAccess};
use sb_proto::ContentKind;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let access = ArboardAccess::new();

    // --data <uti> <파일>: 그 파일 내용을 해당 UTI 데이터로 올린다(앱에서 복사한 상태 재현).
    #[cfg(target_os = "macos")]
    if args.first().map(|a| a == "--data").unwrap_or(false) && args.len() >= 3 {
        let bytes = std::fs::read(&args[2]).expect("파일 읽기");
        let ok = sb_clipboard::macos::write_custom_data(&args[1], &bytes);
        println!("{} 데이터 {}B 올림: {ok}", args[1], bytes.len());
        println!("타입 목록: {:?}", sb_clipboard::macos::available_types());
        println!();
    }

    // --type <uti>: 지원하지 않는 형식을 흉내내 올린다(macOS, 진단 검증용).
    #[cfg(target_os = "macos")]
    if args.first().map(|a| a == "--type").unwrap_or(false) && args.len() >= 2 {
        let ok = sb_clipboard::macos::write_custom_type(&args[1], "x");
        println!("커스텀 타입 {} 올림: {ok}", args[1]);
        println!("타입 목록: {:?}", sb_clipboard::macos::available_types());
        println!("안내 문구: {:?}", sb_clipboard::macos::unsupported_message());
        return;
    }

    // --legacy: 레거시 NSFilenamesPboardType 만 올린다(직접 만든 앱이 흔히 쓰는 방식).
    #[cfg(target_os = "macos")]
    if args.first().map(|a| a == "--legacy").unwrap_or(false) && args.len() >= 2 {
        let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
        let ok = sb_clipboard::macos::write_legacy_filenames(&paths);
        println!("레거시 NSFilenamesPboardType 로 올림: {ok}");
        println!("타입 목록: {:?}", sb_clipboard::macos::available_types());
        println!();
    }

    // --ref: Finder 처럼 "파일 참조 URL"(file:///.file/id=…) 형태로 올린다(macOS).
    let as_ref = args.first().map(|a| a == "--ref").unwrap_or(false);
    let is_data = args
        .first()
        .map(|a| a == "--data" || a == "--legacy")
        .unwrap_or(false);
    let args: Vec<String> = if as_ref { args[1..].to_vec() } else { args };

    if !args.is_empty() && !is_data {
        let paths: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
        if as_ref {
            #[cfg(target_os = "macos")]
            {
                let ok = sb_clipboard::macos::write_file_reference_urls(&paths);
                println!("파일 참조 URL 로 올림: {ok}");
            }
            #[cfg(not(target_os = "macos"))]
            println!("--ref 는 macOS 전용");
        } else {
            match access.write_file_paths(&paths) {
                Ok(()) => println!("클립보드에 파일 {}개 올림", paths.len()),
                Err(e) => println!("실패: {e}"),
            }
        }
        println!();
    }

    println!("파일 클립보드 지원: {}", files::files_supported());
    #[cfg(target_os = "macos")]
    {
        println!("클립보드 타입 목록: {:?}", sb_clipboard::macos::available_types());
        println!("민감 콘텐츠 표시: {}", sb_clipboard::macos::is_concealed_now());
    }
    let raw = files::clipboard_file_paths();
    println!("클립보드의 파일 경로 {}개:", raw.len());
    for p in &raw {
        println!("  {}", p.display());
    }

    match access.read() {
        Ok(Some(c)) => {
            print!("read() → {:?} {} bytes", c.kind, c.bytes.len());
            match c.kind {
                ContentKind::Files => match files::bundle_from_bytes(&c.bytes) {
                    Ok(b) => println!(" · {}", b.preview()),
                    Err(e) => println!(" · 번들 해석 실패: {e}"),
                },
                ContentKind::Text => {
                    let s = String::from_utf8_lossy(&c.bytes);
                    println!(" · {:?}", s.chars().take(60).collect::<String>());
                }
                ContentKind::ImagePng => println!(),
            }
        }
        Ok(None) => println!("read() → 빈 클립보드"),
        Err(e) => println!("read() 실패: {e}"),
    }
}
