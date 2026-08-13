# Linux 전 기능 검증용 이미지 — macOS 에서 확인 못 하는 경로(Wayland/X11 클립보드, webkit2gtk
# 웹뷰, 트레이, glibc 링크)를 컨테이너 안에서 실제로 돌려 본다.
#
#   docker build -t shareboard-verify -f scripts/docker/Dockerfile scripts/docker
#
# 소스는 이미지에 넣지 않는다 — 런타임에 호스트 저장소를 읽기 전용으로 마운트하고
# 컨테이너 전용 작업 볼륨으로 복사해 쓴다(호스트 target/ · node_modules 오염 방지).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    CARGO_HOME=/opt/cargo \
    RUSTUP_HOME=/opt/rustup \
    PATH=/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git rsync build-essential pkg-config file jq \
      procps psmisc iproute2 lsof xz-utils unzip \
      # sb-clipboard: arboard(X11/xcb) + wl-clipboard-rs(Wayland) 빌드 의존성
      libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libwayland-dev \
      # Tauri v2 데스크톱 앱 빌드 의존성
      libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
      libayatana-appindicator3-dev librsvg2-dev libssl-dev libgtk-3-dev \
      # UI 검증: 가상 디스플레이 · 세션 버스 · WebDriver · 스크린샷 · X11 클립보드 도구
      xvfb dbus-x11 webkit2gtk-driver imagemagick x11-utils x11-apps xclip xdotool \
      at-spi2-core \
      # 한글 UI 가 두부(□)로 렌더되지 않도록
      fonts-noto-cjk fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# Rust (워크스페이스 rust-version = 1.82 이상)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain stable -c rustfmt -c clippy \
    && rustc --version && cargo --version

# Node 22 + pnpm(package.json packageManager 필드 버전을 corepack 이 맞춘다)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && corepack enable \
    && node --version

# 실제 Tauri 앱을 WebDriver 로 조작하기 위한 브리지(Linux 는 WebKitWebDriver 를 감싼다).
RUN cargo install tauri-driver --locked --version '^2' \
    && tauri-driver --help >/dev/null 2>&1 || true

WORKDIR /w
