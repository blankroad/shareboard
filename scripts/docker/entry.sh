#!/usr/bin/env bash
# 컨테이너 진입점 — 호스트 저장소(/src, 읽기 전용)를 컨테이너 작업 사본(/w)으로 동기화한 뒤
# verify.sh 로 넘긴다. target/ · node_modules 는 컨테이너 쪽 것을 유지해야 하므로 제외한다
# (rsync 는 --exclude 대상 파일을 --delete 에서도 보호한다).
set -euo pipefail

rsync -a --delete \
  --exclude '.git/' \
  --exclude 'target/' \
  --exclude 'src-tauri/target/' \
  --exclude 'node_modules/' \
  --exclude 'dist/' \
  --exclude '.target-*/' \
  --exclude 'verify-out/' \
  /src/ /w/

exec bash /w/scripts/docker/verify.sh "$@"
