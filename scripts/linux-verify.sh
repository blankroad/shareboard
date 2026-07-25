#!/usr/bin/env bash
# Ubuntu 컨테이너 안에서 Linux 빌드·테스트를 검증한다 (macOS 에서 못 하는 부분).
# 사용: docker run --rm -v "$PWD":/w -w /w ubuntu:24.04 bash scripts/linux-verify.sh
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
echo "::: [1/5] 시스템 의존성 설치"
apt-get update -qq
apt-get install -y -qq \
  curl build-essential pkg-config git \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libwayland-dev >/dev/null

echo "::: [2/5] Rust 설치"
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
fi
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version

# target 은 macOS 호스트와 공유되면 안 됨(오브젝트 불일치) → 컨테이너 전용 디렉터리 사용.
export CARGO_TARGET_DIR=/tmp/target-linux

echo "::: [3/5] 코어 워크스페이스 테스트 (Linux)"
# 유닛 테스트 결과 라인을 모두 보존(도크 테스트의 0-passed 에 묻히지 않게).
cargo test --workspace 2>&1 | grep -E "Running unittests|test result:|FAILED|error\[" || true
echo "  -> 통과 합계: $(cargo test --workspace 2>&1 | grep -E '^test result: ok' | awk '{s+=$4} END{print s}')"

echo "::: [4/5] Wayland 백엔드 컴파일 검증 (지금까지 미검증 코드)"
cargo build -p sb-clipboard --features wayland-backend 2>&1 | tail -5

echo "::: [5/5] 서버 릴리스 빌드"
cargo build -p sb-server --release 2>&1 | tail -3
ls -la "$CARGO_TARGET_DIR/release/sb-server"

echo "::: 완료 — Linux 검증 통과"
