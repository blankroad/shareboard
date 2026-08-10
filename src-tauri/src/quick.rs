//! 히스토리 빠른 열기 — 글로벌 핫키로 토글하는 팝업 창(label `quick`, `index.html#quick`).
//!
//! 창은 tauri.conf.json 에 숨겨진 상태로 선언돼 있고, 핫키·트레이 메뉴가 토글한다.
//! 포커스를 잃으면 자동으로 숨는다(main.rs 의 on_window_event) — 런처 관례.

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub const LABEL: &str = "quick";

/// 팝업 토글. 보이면 숨기고, 숨어 있으면 화면 중앙에 띄우고 포커스를 준다.
pub fn toggle(app: &AppHandle) {
    let Some(w) = app.get_webview_window(LABEL) else {
        tracing::warn!("quick 창이 없습니다");
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return;
    }
    let _ = w.center();
    let _ = w.show();
    let _ = w.set_focus();
    // 패널이 검색어를 비우고 최신 히스토리를 다시 읽도록.
    let _ = w.emit("quick-opened", ());
}

/// 설정의 accelerator 로 핫키를 (재)등록한다. 빈 문자열이면 등록하지 않는다(= 핫키 끔).
///
/// 실패는 대개 다른 앱이 같은 조합을 이미 잡고 있는 경우다.
pub fn apply_hotkey(app: &AppHandle, accel: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let accel = accel.trim();
    if accel.is_empty() {
        return Ok(());
    }
    gs.on_shortcut(accel, |app, _shortcut, event| {
        // 누를 때만 반응 — 뗄 때 중복 토글 방지.
        if event.state == ShortcutState::Pressed {
            toggle(app);
        }
    })
    .map_err(|e| format!("핫키 '{accel}' 등록 실패 (다른 앱이 쓰는 조합일 수 있습니다): {e}"))
}
