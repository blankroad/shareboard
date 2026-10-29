//! Linux 파일 클립보드 — X11 `CLIPBOARD` selection 을 직접 다룬다(Wayland 세션은 XWayland 경유).
//!
//! arboard 는 파일 목록을 `text/uri-list` 하나로만 올린다. KDE Dolphin 은 그걸로 붙여넣지만
//! GNOME Files(Nautilus)·Nemo·Caja·Thunar 는 `x-special/gnome-copied-files` 를 보므로, 그 형식이
//! 없으면 붙여넣기가 비활성으로 남는다. 한 selection 에 소유자는 하나뿐이라 arboard 위에 형식을
//! 덧붙일 수 없어서, 파일 클립은 여기서 직접 소유자가 되어 세 형식을 모두 제공한다.
//!
//! - 읽기: `TARGETS` 를 보고 `text/uri-list` → 없으면 `x-special/gnome-copied-files`.
//! - 쓰기: 전용 창으로 selection 을 소유하고, 백그라운드 스레드가 요청(`SelectionRequest`)에
//!   응답한다. 다른 앱(또는 arboard 의 텍스트 쓰기)이 소유권을 가져가면 `SelectionClear` 로 놓는다.
//!
//! Linux 는 클립보드 내용이 **소유 프로세스 안에 산다** — 앱이 켜져 있는 동안만 붙여넣기가 된다
//! (arboard 텍스트와 같은 조건).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, GetPropertyReply, PropMode,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use crate::uri_list;
use crate::ClipError;

/// 다른 앱이 selection 요청에 응답하기를 기다리는 최대 시간. 폴링(400ms)보다 짧아야 한다.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
struct Atoms {
    clipboard: Atom,
    targets: Atom,
    uri_list: Atom,
    gnome: Atom,
    utf8: Atom,
    text_plain: Atom,
    incr: Atom,
    /// 읽기 결과를 받을 우리 창의 속성 이름.
    prop: Atom,
}

struct X {
    conn: RustConnection,
    win: Window,
    atoms: Atoms,
}

fn xerr<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn open_x() -> Result<X, String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(xerr)?;
    let screen = &conn.setup().roots[screen_num];
    let (root, visual) = (screen.root, screen.root_visual);
    let win = conn.generate_id().map_err(xerr)?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(xerr)?;
    let intern = |name: &[u8]| -> Result<Atom, String> {
        Ok(conn
            .intern_atom(false, name)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?
            .atom)
    };
    let atoms = Atoms {
        clipboard: intern(b"CLIPBOARD")?,
        targets: intern(b"TARGETS")?,
        uri_list: intern(b"text/uri-list")?,
        gnome: intern(b"x-special/gnome-copied-files")?,
        utf8: intern(b"UTF8_STRING")?,
        text_plain: intern(b"text/plain;charset=utf-8")?,
        incr: intern(b"INCR")?,
        prop: intern(b"SHAREBOARD_SEL")?,
    };
    conn.flush().map_err(xerr)?;
    Ok(X { conn, win, atoms })
}

// ── 읽기 ────────────────────────────────────────────────────────────────

/// 읽기 전용 연결(폴링마다 새로 열지 않는다). X 디스플레이가 없으면 `None` — 이후 시도도 안 한다.
fn reader() -> &'static Mutex<Option<X>> {
    static READER: OnceLock<Mutex<Option<X>>> = OnceLock::new();
    READER.get_or_init(|| {
        Mutex::new(match open_x() {
            Ok(x) => Some(x),
            Err(e) => {
                tracing::warn!("X11 연결 실패 — Linux 파일 클립보드를 쓸 수 없습니다: {e}");
                None
            }
        })
    })
}

/// 현재 소유자에게 `target` 형식을 요청해 받는다. 거절·시간 초과·INCR(대용량)이면 `None`.
fn read_target(x: &X, target: Atom) -> Option<GetPropertyReply> {
    let a = x.atoms;
    x.conn
        .convert_selection(x.win, a.clipboard, target, a.prop, CURRENT_TIME)
        .ok()?;
    x.conn.flush().ok()?;
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        match x.conn.poll_for_event() {
            // 이전 요청이 늦게 도착한 응답일 수 있어 target 까지 맞춰 본다.
            Ok(Some(Event::SelectionNotify(e))) if e.requestor == x.win && e.target == target => {
                if e.property == NONE {
                    return None; // 소유자가 이 형식을 거절
                }
                let r = x
                    .conn
                    .get_property(true, x.win, a.prop, AtomEnum::ANY, 0, 1 << 20)
                    .ok()?
                    .reply()
                    .ok()?;
                // 파일 목록은 작다 — INCR 분할 전송은 지원하지 않는다.
                return (r.type_ != a.incr).then_some(r);
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(3));
            }
            Err(_) => return None,
        }
    }
}

/// 클립보드의 파일 경로. 파일 클립이 아니거나 X 를 못 쓰면 빈 Vec.
pub fn read_file_paths() -> Vec<PathBuf> {
    let Ok(guard) = reader().lock() else {
        return Vec::new();
    };
    let Some(x) = guard.as_ref() else {
        return Vec::new();
    };
    let a = x.atoms;
    let owner = x
        .conn
        .get_selection_owner(a.clipboard)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.owner)
        .unwrap_or(NONE);
    if owner == NONE {
        return Vec::new();
    }
    let Some(targets) = read_target(x, a.targets) else {
        return Vec::new();
    };
    let offered: Vec<Atom> = targets.value32().map(|it| it.collect()).unwrap_or_default();
    for want in [a.uri_list, a.gnome] {
        if offered.contains(&want) {
            if let Some(r) = read_target(x, want) {
                let paths = uri_list::parse(&r.value);
                if !paths.is_empty() {
                    return paths;
                }
            }
        }
    }
    Vec::new()
}

// ── 쓰기(소유자) ────────────────────────────────────────────────────────

struct Offer {
    uri_list: Vec<u8>,
    gnome: Vec<u8>,
    text: Vec<u8>,
}

struct Owner {
    x: Arc<X>,
    offer: Arc<Mutex<Option<Offer>>>,
}

fn owner() -> Result<&'static Owner, ClipError> {
    static OWNER: OnceLock<Result<Owner, String>> = OnceLock::new();
    OWNER
        .get_or_init(|| {
            let x = Arc::new(open_x()?);
            let offer: Arc<Mutex<Option<Offer>>> = Arc::new(Mutex::new(None));
            let (tx, toffer) = (x.clone(), offer.clone());
            std::thread::Builder::new()
                .name("sb-x11-files".into())
                .spawn(move || serve(&tx, &toffer))
                .map_err(xerr)?;
            Ok(Owner { x, offer })
        })
        .as_ref()
        .map_err(|e| ClipError::Access(format!("X11 연결 실패: {e}")))
}

/// selection 요청 응답 루프(앱 수명 동안).
fn serve(x: &X, offer: &Mutex<Option<Offer>>) {
    loop {
        match x.conn.wait_for_event() {
            Ok(Event::SelectionRequest(ev)) => respond(x, offer, ev),
            Ok(Event::SelectionClear(ev)) if ev.selection == x.atoms.clipboard => {
                // 다른 앱이 복사했다 — 우리 제공을 내린다.
                if let Ok(mut o) = offer.lock() {
                    *o = None;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("X11 파일 클립보드 연결 끊김: {e}");
                return;
            }
        }
    }
}

fn respond(x: &X, offer: &Mutex<Option<Offer>>, ev: SelectionRequestEvent) {
    let a = x.atoms;
    // 구식 클라이언트는 property 를 NONE 으로 보낸다 — 그때는 target 이름을 속성으로 쓴다(ICCCM).
    let property = if ev.property == NONE {
        ev.target
    } else {
        ev.property
    };
    let ok = (|| -> Option<()> {
        let guard = offer.lock().ok()?;
        let o = guard.as_ref()?;
        if ev.selection != a.clipboard {
            return None;
        }
        let c = &x.conn;
        let r = if ev.target == a.targets {
            c.change_property32(
                PropMode::REPLACE,
                ev.requestor,
                property,
                AtomEnum::ATOM,
                &[a.targets, a.uri_list, a.gnome, a.utf8, a.text_plain],
            )
        } else if ev.target == a.uri_list {
            c.change_property8(PropMode::REPLACE, ev.requestor, property, a.uri_list, &o.uri_list)
        } else if ev.target == a.gnome {
            c.change_property8(PropMode::REPLACE, ev.requestor, property, a.gnome, &o.gnome)
        } else if ev.target == a.utf8 || ev.target == a.text_plain {
            c.change_property8(PropMode::REPLACE, ev.requestor, property, ev.target, &o.text)
        } else {
            return None;
        };
        r.ok().map(|_| ())
    })()
    .is_some();

    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: ev.time,
        requestor: ev.requestor,
        selection: ev.selection,
        target: ev.target,
        property: if ok { property } else { NONE },
    };
    let _ = x
        .conn
        .send_event(false, ev.requestor, EventMask::NO_EVENT, notify);
    let _ = x.conn.flush();
}

/// 경로들을 파일 클립으로 올린다(Nautilus·Dolphin·Nemo·Thunar 에서 붙여넣기 가능).
pub fn write_file_paths(paths: &[PathBuf]) -> Result<(), ClipError> {
    if paths.is_empty() {
        return Err(ClipError::Unsupported);
    }
    let o = owner()?;
    let text = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    *o.offer
        .lock()
        .map_err(|_| ClipError::Access("클립보드 잠금 오염".into()))? = Some(Offer {
        uri_list: uri_list::to_uri_list(paths).into_bytes(),
        gnome: uri_list::to_gnome_copied_files(paths).into_bytes(),
        text: text.into_bytes(),
    });
    let x = &o.x;
    let a = x.atoms;
    let err = |e: String| ClipError::Access(format!("X11 selection 쓰기 실패: {e}"));
    x.conn
        .set_selection_owner(x.win, a.clipboard, CURRENT_TIME)
        .map_err(|e| err(e.to_string()))?;
    let now = x
        .conn
        .get_selection_owner(a.clipboard)
        .map_err(|e| err(e.to_string()))?
        .reply()
        .map_err(|e| err(e.to_string()))?
        .owner;
    if now != x.win {
        return Err(err("소유권을 얻지 못했습니다".into()));
    }
    Ok(())
}

/// 실제 X 서버 대상 검증(DISPLAY + xclip 필요 — 없으면 건너뛴다). Docker 검증에서 Xvfb 로 돈다.
/// 테스트끼리 같은 CLIPBOARD 를 쓰므로 한 테스트 안에서 순서대로 확인한다.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn xclip_ok() -> bool {
        std::env::var_os("DISPLAY").is_some()
            && Command::new("xclip")
                .arg("-version")
                .stderr(Stdio::null())
                .status()
                .is_ok()
    }

    fn xclip_out(target: &str) -> String {
        let o = Command::new("xclip")
            .args(["-o", "-selection", "clipboard", "-t", target])
            .output()
            .expect("xclip");
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    #[test]
    fn interoperates_with_other_x11_apps() {
        if !xclip_ok() {
            eprintln!("DISPLAY/xclip 없음 — 건너뜀");
            return;
        }
        let paths = vec![
            PathBuf::from("/tmp/sb x11/보고서 (최종).pdf"),
            PathBuf::from("/tmp/b.png"),
        ];

        // 1) 우리가 올린 파일 클립을 다른 앱(xclip)이 세 형식으로 읽는다.
        write_file_paths(&paths).expect("쓰기");
        let targets = xclip_out("TARGETS");
        for t in ["text/uri-list", "x-special/gnome-copied-files", "UTF8_STRING"] {
            assert!(targets.contains(t), "TARGETS 에 {t} 없음: {targets}");
        }
        let gnome = xclip_out("x-special/gnome-copied-files");
        assert!(gnome.starts_with("copy\nfile:///tmp/sb%20x11/"), "{gnome}");
        assert_eq!(uri_list::parse(gnome.as_bytes()), paths);
        assert_eq!(uri_list::parse(xclip_out("text/uri-list").as_bytes()), paths);
        assert_eq!(
            xclip_out("UTF8_STRING"),
            "/tmp/sb x11/보고서 (최종).pdf\n/tmp/b.png"
        );

        // 2) 우리 읽기 경로도 (별도 연결로) 자기 클립을 읽는다.
        assert_eq!(read_file_paths(), paths);

        // 3) 다른 앱이 uri-list 로 복사한 파일을 읽는다(KDE·GTK4 방식). 소유권이 넘어가면
        //    우리 제공은 내려간다.
        let mut child = Command::new("xclip")
            .args(["-i", "-selection", "clipboard", "-t", "text/uri-list"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip -i");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"file:///home/u/%ED%95%9C%EA%B8%80.txt\r\n")
            .unwrap();
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(read_file_paths(), vec![PathBuf::from("/home/u/한글.txt")]);
        assert!(
            owner().unwrap().offer.lock().unwrap().is_none(),
            "SelectionClear 로 우리 제공을 내려야 한다"
        );

        // 4) GNOME 형식만 있는 클립(구버전 Nautilus)도 읽는다.
        let mut child = Command::new("xclip")
            .args([
                "-i",
                "-selection",
                "clipboard",
                "-t",
                "x-special/gnome-copied-files",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip -i");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"copy\nfile:///tmp/g.txt")
            .unwrap();
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(read_file_paths(), vec![PathBuf::from("/tmp/g.txt")]);

        // 5) 텍스트만 있는 클립은 파일이 아니다.
        let mut child = Command::new("xclip")
            .args(["-i", "-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("xclip -i");
        child.stdin.take().unwrap().write_all(b"just text").unwrap();
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(read_file_paths().is_empty());
    }
}
