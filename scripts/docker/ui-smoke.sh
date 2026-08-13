#!/usr/bin/env bash
# 실제 Tauri 앱을 가상 디스플레이(Xvfb)에서 띄우고 WebDriver 로 UI 를 조작한다.
#
#   ui-smoke.sh single   — 1인스턴스: 온보딩 → 호스팅 → 탭 전체 → 초대 → 클립보드 캡처 → 팝업
#   ui-smoke.sh two      — 2인스턴스: 호스트 → 초대 링크 → 참여 → 양방향 클립보드 동기화 → 강퇴
#
# 인스턴스마다 **디스플레이를 분리**한다 — X 클립보드는 디스플레이 단위라, 같은 화면에 두 앱을
# 띄우면 "네트워크로 동기화된 것"과 "같은 클립보드를 둘 다 본 것"을 구분할 수 없다.
set -uo pipefail

MODE=${1:-single}
OUT=${OUT:-/out}
# 릴리스 바이너리여야 한다 — 디버그 빌드는 devUrl(localhost:5173)을 로드해 오류 페이지가 뜬다.
APP=${APP:-/w/.target-tauri/release/shareboard}
mkdir -p "$OUT/shots"

[ -x "$APP" ] || {
  echo "앱 바이너리가 없다: $APP (phase app 을 먼저 돌려야 한다)"
  exit 2
}

PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  sleep 0.3
  for p in "${PIDS[@]:-}"; do kill -9 "$p" 2>/dev/null; done
  pkill -f 'tauri-driver' 2>/dev/null
  pkill -f 'WebKitWebDriver' 2>/dev/null
  pkill -f "$APP" 2>/dev/null
  pkill xclip 2>/dev/null
  true
}
trap cleanup EXIT
# 앞선 실행이 중간에 죽었으면 드라이버가 세션을 쥔 채 남아 "Maximum number of active
# sessions" 로 새 세션이 거부된다 — 시작 시점에도 한 번 쓸어 낸다.
cleanup
sleep 1

# 자동 시작 플러그인은 ~/.config/autostart 에 .desktop 을 쓰는데, 상위 디렉터리가 없으면
# "No such file or directory" 로 실패한다. 실제 데스크톱에는 늘 있지만 컨테이너에는 없다.
mkdir -p "$HOME/.config"

# 트레이(appindicator)·webkit 이 세션 버스를 찾는다.
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  eval "$(dbus-launch --sh-syntax)"
  export DBUS_SESSION_BUS_ADDRESS DBUS_SESSION_BUS_PID
fi

start_x() { # start_x <:disp>
  local d=$1
  rm -f "/tmp/.X${d#:}-lock"
  Xvfb "$d" -screen 0 1280x900x24 -nolisten tcp >"$OUT/xvfb${d#:}.log" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 60); do
    DISPLAY="$d" xdpyinfo >/dev/null 2>&1 && {
      echo "  Xvfb $d 준비됨"
      return 0
    }
    sleep 0.2
  done
  echo "Xvfb $d 기동 실패"
  cat "$OUT/xvfb${d#:}.log"
  return 1
}

start_driver() { # start_driver <port> <native_port> <:disp> <datadir> <tag>
  local port=$1 native=$2 disp=$3 dd=$4 tag=$5
  mkdir -p "$dd"
  env DISPLAY="$disp" \
    SHAREBOARD_DATA_DIR="$dd" \
    RUST_LOG=info \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    GDK_BACKEND=x11 \
    NO_AT_BRIDGE=1 \
    tauri-driver --port "$port" --native-port "$native" >"$OUT/driver-$tag.log" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 60); do
    if ss -ltn 2>/dev/null | grep -q ":$port "; then
      echo "  tauri-driver($tag) :$port 준비됨"
      return 0
    fi
    sleep 0.25
  done
  echo "tauri-driver($tag) 기동 실패"
  cat "$OUT/driver-$tag.log"
  return 1
}

# 전체 화면 캡처 — 웹뷰 밖(창 테두리·트레이 유무)까지 눈으로 확인할 수 있게 남긴다.
grab() { # grab <:disp> <이름>
  import -display "$1" -window root "$OUT/shots/$2.png" 2>/dev/null &&
    echo "  📸 $2.png ($(identify -format '%wx%h σ=%[fx:standard_deviation]' "$OUT/shots/$2.png" 2>/dev/null))"
}
export -f grab
export OUT

echo "=== UI 스모크 모드: $MODE"

rm -rf /w/.smoke/ui && mkdir -p /w/.smoke/ui

if [ "$MODE" = "single" ] || [ "$MODE" = "probe" ]; then
  start_x :98 || exit 1
  start_driver 4444 4445 :98 /w/.smoke/ui/a A || exit 1
  DISPLAY=:98 WD=http://127.0.0.1:4444 APP="$APP" DISP=:98 \
    node /w/scripts/docker/ui-smoke.mjs "$MODE"
  rc=$?
  grab :98 "zz-final-$MODE"
  exit $rc
else
  start_x :98 || exit 1
  start_x :99 || exit 1
  start_driver 4444 4445 :98 /w/.smoke/ui/a A || exit 1
  start_driver 4454 4455 :99 /w/.smoke/ui/b B || exit 1
  WD_A=http://127.0.0.1:4444 WD_B=http://127.0.0.1:4454 \
    DISP_A=:98 DISP_B=:99 APP="$APP" \
    node /w/scripts/docker/ui-smoke.mjs two
  rc=$?
  grab :98 zz-final-A
  grab :99 zz-final-B
  exit $rc
fi
