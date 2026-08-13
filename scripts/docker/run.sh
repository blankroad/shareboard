#!/usr/bin/env bash
# 호스트(macOS)에서 Linux 전 기능 검증을 돌린다.
#
#   scripts/docker/run.sh                 # 전 단계
#   scripts/docker/run.sh core e2e        # 일부 단계만
#   단계: core(빌드·테스트) e2e(프로토콜) front(프런트엔드) app(앱 빌드) ui(UI 스모크) two(2인스턴스)
#
# 로그·스크린샷은 verify-out/ 에 떨어진다. 컨테이너 작업 사본과 빌드 캐시는 도커 볼륨에
# 남으므로(호스트 target/ 은 건드리지 않는다) 두 번째 실행부터는 증분 빌드다.
#
# 이미지 빌드가 "load metadata for docker.io/…" 에서 멈춘다면 Docker Desktop 자격증명
# 헬퍼(credsStore=desktop)가 응답하지 않는 상태다. auths 만 있는 임시 설정으로 우회한다:
#   mkdir -p /tmp/dcfg && echo '{"auths":{}}' > /tmp/dcfg/config.json
#   DOCKER_CONFIG=/tmp/dcfg DOCKER_BUILDKIT=0 scripts/docker/run.sh
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
OUT=${OUT:-$REPO/verify-out}
IMAGE=${IMAGE:-shareboard-verify}

mkdir -p "$OUT"
docker image inspect "$IMAGE" >/dev/null 2>&1 ||
  docker build -t "$IMAGE" -f "$REPO/scripts/docker/Dockerfile" "$REPO/scripts/docker"

docker volume create sb-verify-work >/dev/null
docker volume create sb-verify-cargo >/dev/null

TTY=()
[ -t 1 ] && TTY=(-t)

exec docker run --rm "${TTY[@]}" \
  --name shareboard-verify-run \
  --security-opt seccomp=unconfined \
  --shm-size=1g \
  -v "$REPO":/src:ro \
  -v sb-verify-work:/w \
  -v sb-verify-cargo:/opt/cargo/registry \
  -v "$OUT":/out \
  "$IMAGE" bash /src/scripts/docker/entry.sh "$@"
