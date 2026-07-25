#!/usr/bin/env bash
# shareboard 아이콘 생성 파이프라인 (PLAN.md §9).
# 마스터 SVG → 1024 PNG(resvg) → `cargo tauri icon` 으로 플랫폼 아이콘 세트 생성.
# 트레이 상태별 PNG 는 별도로 생성한다.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/assets/icons"
OUT="$ROOT/src-tauri/icons"
TRAY="$OUT/tray"

command -v resvg >/dev/null || { echo "resvg 필요: brew install resvg / cargo install resvg"; exit 1; }
mkdir -p "$OUT" "$TRAY"

echo "[1/3] 마스터 앱 아이콘 1024 PNG"
resvg --width 1024 --height 1024 "$SRC/app-icon.svg" "$SRC/app-icon-1024.png"

echo "[2/3] 플랫폼 아이콘 세트 (cargo tauri icon)"
if command -v cargo-tauri >/dev/null || cargo tauri --version >/dev/null 2>&1; then
  (cd "$ROOT/src-tauri" && cargo tauri icon "$SRC/app-icon-1024.png")
else
  echo "  (cargo-tauri 미설치 — 세트 생성 건너뜀. 'cargo install tauri-cli' 후 재실행)"
fi

echo "[3/3] 트레이 상태별 PNG (16/22/32/44)"
for sz in 16 22 32 44; do
  resvg --width "$sz" --height "$sz" "$SRC/tray-template.svg" "$TRAY/tray-template-${sz}.png"
done

echo "완료. 앱 아이콘: $OUT, 트레이: $TRAY"
