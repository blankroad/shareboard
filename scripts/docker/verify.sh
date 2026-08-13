#!/usr/bin/env bash
# 컨테이너(Ubuntu 24.04) 안에서 도는 전 기능 스모크 검증.
#
#   verify.sh [core] [e2e] [front] [app] [ui] [two]     (인자 없으면 전부)
#
# 각 스텝 로그는 $OUT(기본 /out)에 남고, 마지막에 표로 요약한다.
# 실패해도 계속 진행해 "무엇이 깨졌는지"를 한 번에 모은다.
set -uo pipefail

OUT=${OUT:-/out}
mkdir -p "$OUT"

export CARGO_TERM_COLOR=never
export RUST_BACKTRACE=1
# corepack 이 pnpm 을 내려받을 때 물어보지 않게(비대화 실행).
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
export CI=1
# 호스트(macOS) target/ 과 오브젝트가 섞이면 안 되므로 컨테이너 전용 디렉터리를 쓴다.
export CARGO_TARGET_DIR=/w/.target-linux
TAURI_TARGET=/w/.target-tauri
CORE_REL="$CARGO_TARGET_DIR/release"

PHASES=("$@")
[ ${#PHASES[@]} -eq 0 ] && PHASES=(core e2e front app ui two)
has() { for p in "${PHASES[@]}"; do [ "$p" = "$1" ] && return 0; done; return 1; }

STEP_NAMES=()
STEP_RESULTS=()

step() { # step <이름> <명령...>
  local name="$1"
  shift
  local log="$OUT/${name}.log"
  printf '── %-28s ' "$name"
  local t0
  t0=$(date +%s)
  if "$@" >"$log" 2>&1; then
    printf '✅ PASS (%ss)\n' "$(($(date +%s) - t0))"
    STEP_NAMES+=("$name")
    STEP_RESULTS+=("PASS")
  else
    local rc=$?
    printf '❌ FAIL rc=%s (%ss)\n' "$rc" "$(($(date +%s) - t0))"
    tail -n 25 "$log" | sed 's/^/      │ /'
    STEP_NAMES+=("$name")
    STEP_RESULTS+=("FAIL(rc=$rc)")
  fi
}

# 실패를 기대하는 스텝(부정 테스트) — 성공하면 그게 버그다.
step_expect_fail() { # step_expect_fail <이름> <명령...>
  local name="$1"
  shift
  local log="$OUT/${name}.log"
  printf '── %-28s ' "$name"
  if "$@" >"$log" 2>&1; then
    printf '❌ FAIL (거부되어야 하는데 성공함)\n'
    tail -n 15 "$log" | sed 's/^/      │ /'
    STEP_NAMES+=("$name")
    STEP_RESULTS+=("FAIL(거부 안 됨)")
  else
    printf '✅ PASS (기대대로 거부)\n'
    STEP_NAMES+=("$name")
    STEP_RESULTS+=("PASS")
  fi
}

banner() { echo; echo "═══ $1"; }

# ───────────────────────────────────────────────────────── phase: core
core_test() { cd /w && cargo test --workspace; }
core_fmt() { cd /w && cargo fmt --all --check; }
core_clippy() { cd /w && cargo clippy --workspace --all-targets; }
core_wayland() { cd /w && cargo build -p sb-clipboard --features wayland-backend; }
core_server_release() { cd /w && cargo build -p sb-server --release && ls -la "$CORE_REL/sb-server"; }

phase_core() {
  banner "Phase 1 — 코어 워크스페이스 (Linux)"
  step core-fmt core_fmt
  step core-clippy core_clippy
  step core-test core_test
  step core-wayland-backend core_wayland
  step core-server-release core_server_release
}

# ───────────────────────────────────────────────────────── phase: e2e
SRV_DIR=/w/.smoke/server

e2e_two_client() { cd /w && cargo run -q -p sb-server --example two_client_sync; }
e2e_revoke() { cd /w && cargo run -q -p sb-server --example three_client_revoke; }

# LAN allowlist(§4.5) — 공인 IP 바인딩은 거부되어야 한다.
e2e_lan_guard() {
  rm -rf /w/.smoke/languard && mkdir -p /w/.smoke/languard
  "$CORE_REL/sb-server" --init --bind 8.8.8.8:45871 \
    --data-dir /w/.smoke/languard/data --config /w/.smoke/languard/server.toml
}

server_start() { # server_start <로그파일>
  ("$CORE_REL/sb-server" --config "$SRV_DIR/server.toml" >"$1" 2>&1 &
    echo $! >"$SRV_DIR/pid")
  for _ in $(seq 1 50); do
    ss -ltn 2>/dev/null | grep -q ':45871 ' && return 0
    sleep 0.2
  done
  echo "서버가 45871 을 열지 못함"
  cat "$1"
  return 1
}
server_stop() {
  [ -f "$SRV_DIR/pid" ] && kill "$(cat "$SRV_DIR/pid")" 2>/dev/null
  for _ in $(seq 1 25); do
    ss -ltn 2>/dev/null | grep -q ':45871 ' || return 0
    sleep 0.2
  done
  return 0
}

# 실제 sb-server 기동 → smoke_client 로 TLS 지문 pinning + mTLS + ClaimWorkspace + Welcome.
e2e_live_server() {
  set -e
  rm -rf "$SRV_DIR"
  mkdir -p "$SRV_DIR"
  cd "$SRV_DIR"
  "$CORE_REL/sb-server" --init --bind 127.0.0.1:45871 \
    --data-dir "$SRV_DIR/data" --config "$SRV_DIR/server.toml" | tee "$SRV_DIR/init.txt"

  # --init 출력에서 setup 토큰(② 다음 줄)과 지문을 뽑는다.
  local token fp
  token=$(grep -A1 '^② ' "$SRV_DIR/init.txt" | tail -1 | tr -d ' \r')
  fp=$(grep '서버 지문' "$SRV_DIR/init.txt" | sed 's/.*: *//' | tr -d ' \r')
  echo "token=$token"
  echo "fp=$fp (${#fp}자)"
  [ ${#fp} -eq 64 ] || { echo "지문 길이가 64가 아님"; return 1; }
  [ -n "$token" ] || { echo "setup 토큰 파싱 실패"; return 1; }
  echo "$token" >"$SRV_DIR/token.txt"
  echo "$fp" >"$SRV_DIR/fp.txt"

  server_start "$SRV_DIR/server.log"
  echo "--- 서버 기동 로그"
  cat "$SRV_DIR/server.log"

  echo "--- smoke_client (1회차: 성공해야 함)"
  cd /w
  cargo run -q -p sb-server --example smoke_client -- 127.0.0.1:45871 "$fp" "$token"

  echo "--- smoke_client (2회차: setup 토큰 1회용 → ClaimWorkspace 거부되어야 함)"
  if cargo run -q -p sb-server --example smoke_client -- 127.0.0.1:45871 "$fp" "$token"; then
    echo "❌ 이미 claim 된 워크스페이스를 같은 토큰으로 또 claim 했다"
    server_stop
    return 1
  fi
  echo "  → 기대대로 거부됨"

  echo "--- 잘못된 지문으로 접속 시 pinning 실패해야 함"
  local badfp="00000000000000000000000000000000000000000000000000000000000000ff"
  if cargo run -q -p sb-server --example smoke_client -- 127.0.0.1:45871 "$badfp" "$token"; then
    echo "❌ 지문이 틀린데 연결이 성립했다 (pinning 미작동)"
    server_stop
    return 1
  fi
  echo "  → 기대대로 거부됨"

  echo "--- 서버 재시작 후 워크스페이스 로그 영속 확인"
  server_stop
  server_start "$SRV_DIR/server2.log"
  cat "$SRV_DIR/server2.log"
  grep -q '복원했습니다' "$SRV_DIR/server2.log" || {
    echo "❌ 재시작 후 wslog 복원 메시지가 없다 (멤버십 영속 실패)"
    server_stop
    return 1
  }
  server_stop
  echo "✅ live server 스모크 통과"
}

phase_e2e() {
  banner "Phase 2 — 프로토콜 E2E 스모크"
  step e2e-two-client-sync e2e_two_client
  step e2e-three-client-revoke e2e_revoke
  step_expect_fail e2e-lan-allowlist e2e_lan_guard
  step e2e-live-server e2e_live_server
}

# ───────────────────────────────────────────────────────── phase: front
front_install() { cd /w && pnpm install --frozen-lockfile; }
front_ipc() { cd /w && node scripts/check-ipc-args.mjs; }
front_build() {
  set -e
  cd /w && pnpm build
  test -f /w/dist/index.html
  ls -la /w/dist /w/dist/assets
}

phase_front() {
  banner "Phase 3 — 프런트엔드 + IPC 계약"
  step front-install front_install
  step front-ipc-args front_ipc
  step front-build front_build
}

# ───────────────────────────────────────────────────────── phase: app
# 배포물과 같은 "dist 임베드" 경로로 빌드해야 UI 를 검증할 수 있다.
#
# Tauri v2 는 `dev = !cfg!(feature = "custom-protocol")` 로 판정한다(tauri 2.11 build.rs:257).
# 즉 `cargo build --release` 만으로는 여전히 dev 모드라 devUrl(localhost:5173)을 열고
# 웹뷰에 "Could not connect to localhost" 오류 페이지가 뜬다 — `tauri build` CLI 가 붙여 주는
# `--features tauri/custom-protocol` 을 직접 넣어야 dist 가 임베드된 진짜 앱이 된다.
# (CI 의 app-check 는 `cargo check` 뿐이라 이 경로가 검증되지 않는다.)
#
# 최적화 강도는 동작과 무관하므로 LTO/opt-level 을 낮춰 빌드 시간만 줄인다.
app_build() {
  set -e
  cd /w/src-tauri
  CARGO_TARGET_DIR="$TAURI_TARGET" \
    CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_PROFILE_RELEASE_OPT_LEVEL=1 \
    cargo build --release --features tauri/custom-protocol
  ls -la "$TAURI_TARGET/release/shareboard"
}
phase_app() {
  banner "Phase 4 — 데스크톱 앱 Linux 빌드"
  step app-build app_build
}

# ───────────────────────────────────────────────────────── phase: ui / two
# X11 클립보드 실백엔드(arboard) — macOS/Windows 와 달리 Linux 는 실기기 검증이 비어 있던 경로.
ui_clipboard_backend() {
  set -e
  rm -f /tmp/.X97-lock
  Xvfb :97 -screen 0 800x600x24 -nolisten tcp >/tmp/xvfb97.log 2>&1 &
  local xpid=$!
  export DISPLAY=:97
  for _ in $(seq 1 40); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.25; done

  local rc=0
  local marker="clip-probe-$$-$(date +%s)"
  printf '%s' "$marker" | xclip -selection clipboard -i
  sleep 0.5
  cd /w
  cargo run -q -p sb-clipboard --example clip_probe >/tmp/probe-text.txt 2>&1 || rc=1
  cat /tmp/probe-text.txt
  grep -q "$marker" /tmp/probe-text.txt || {
    echo "❌ X11 텍스트 클립보드를 arboard 가 읽지 못했다"
    rc=1
  }

  convert -size 96x64 xc:'#3366cc' /tmp/clip.png
  xclip -selection clipboard -t image/png -i /tmp/clip.png
  sleep 0.5
  cargo run -q -p sb-clipboard --example clip_probe >/tmp/probe-img.txt 2>&1 || rc=1
  cat /tmp/probe-img.txt
  grep -q "ImagePng" /tmp/probe-img.txt || {
    echo "❌ X11 이미지(PNG) 클립보드를 arboard 가 읽지 못했다"
    rc=1
  }

  kill "$xpid" 2>/dev/null
  return "$rc"
}

# WebDriver 없이도 "리눅스에서 앱이 실제로 뜨고 화면이 그려지는가"만 먼저 못 박는다.
# (webkit2gtk 웹뷰 생성·트레이 등록·창 매핑이 컨테이너에서 되는지의 기본선)
ui_app_boot() {
  set -e
  local dd=/w/.smoke/boot
  rm -rf "$dd" && mkdir -p "$dd"
  rm -f /tmp/.X96-lock
  Xvfb :96 -screen 0 1280x900x24 -nolisten tcp >/tmp/xvfb96.log 2>&1 &
  local xpid=$!
  export DISPLAY=:96
  for _ in $(seq 1 40); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.25; done
  if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    eval "$(dbus-launch --sh-syntax)"
    export DBUS_SESSION_BUS_ADDRESS DBUS_SESSION_BUS_PID
  fi

  local rc=0 applog=/tmp/app-boot.log
  SHAREBOARD_DATA_DIR="$dd" RUST_LOG=info WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 GDK_BACKEND=x11 NO_AT_BRIDGE=1 \
    "$TAURI_TARGET/release/shareboard" >"$applog" 2>&1 &
  local apid=$!

  local win=""
  for _ in $(seq 1 60); do
    win=$(xdotool search --name '^shareboard$' 2>/dev/null | head -1)
    [ -n "$win" ] && break
    kill -0 "$apid" 2>/dev/null || { echo "앱이 죽었다"; cat "$applog"; kill "$xpid"; return 1; }
    sleep 1
  done
  [ -n "$win" ] || { echo "60초 안에 창이 뜨지 않았다"; cat "$applog"; kill "$apid" "$xpid" 2>/dev/null; return 1; }
  echo "창 발견: id=$win  $(xdotool getwindowgeometry "$win" | tr '\n' ' ')"

  sleep 4
  mkdir -p "$OUT/shots"
  import -window root "$OUT/shots/00-boot.png"
  # ImageMagick 6 의 fx 통계는 0~1 정규화 값이다. 완전 단색(빈 화면)이면 0 에 붙는다.
  local sd
  sd=$(identify -format '%[fx:standard_deviation]' "$OUT/shots/00-boot.png")
  echo "화면 표준편차(정규화) = $sd (0 이면 아무것도 안 그려진 것)"
  awk -v s="$sd" 'BEGIN { exit (s > 0.01 ? 0 : 1) }' || {
    echo "❌ 화면이 사실상 비어 있다 — 웹뷰가 렌더되지 않음"
    rc=1
  }

  echo "--- 앱 로그"
  cat "$applog"
  if grep -qE "UI 오류|mount-failed|panicked at|실행 실패" "$applog"; then
    echo "❌ 앱 로그에 치명 오류"
    rc=1
  fi

  kill "$apid" 2>/dev/null
  sleep 1
  kill -9 "$apid" 2>/dev/null
  kill "$xpid" 2>/dev/null
  return "$rc"
}

phase_ui() {
  banner "Phase 5 — 실제 앱 UI 스모크 (Xvfb + WebDriver)"
  step ui-clipboard-backend ui_clipboard_backend
  step ui-app-boot ui_app_boot
  step ui-smoke bash /w/scripts/docker/ui-smoke.sh single
}
phase_two() {
  banner "Phase 6 — 2인스턴스 실사용 시나리오 (호스트 → 초대 → 참여 → 클립보드 동기화)"
  step two-instance bash /w/scripts/docker/ui-smoke.sh two
}

# ───────────────────────────────────────────────────────── run
echo "shareboard Linux 전기능 검증 — $(uname -m) / $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "rustc $(rustc --version | awk '{print $2}') · node $(node --version) · 단계: ${PHASES[*]}"

has core && phase_core
has e2e && phase_e2e
has front && phase_front
has app && phase_app
has ui && phase_ui
has two && phase_two

banner "요약"
fails=0
for i in "${!STEP_NAMES[@]}"; do
  printf '  %-28s %s\n' "${STEP_NAMES[$i]}" "${STEP_RESULTS[$i]}"
  [[ "${STEP_RESULTS[$i]}" == FAIL* ]] && fails=$((fails + 1))
done
echo
if [ "$fails" -eq 0 ]; then
  echo "✅ 전체 통과 (${#STEP_NAMES[@]}개 스텝)"
else
  echo "❌ 실패 $fails / ${#STEP_NAMES[@]} — 로그: $OUT"
fi
exit "$fails"
