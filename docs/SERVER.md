# 서버 설정 가이드

shareboard는 사내망(LAN)에 **서버 1대**를 두고, 각 사람의 앱이 그 서버에 접속해 클립보드를
동기화합니다. 서버는 **암호문만 중계**하며(그룹 키·평문·초대 코드를 절대 갖지 않음), 실제 내용은
멤버들끼리만 아는 그룹 키로 종단간(E2E) 암호화됩니다.

역할은 셋뿐입니다:
- **서버 주소** — 앱이 접속할 곳 (예: `192.168.0.10:45871`)
- **서버 지문** — 접속 상대가 진짜 우리 서버인지 확인하는 값 (모든 멤버에게 공지)
- **setup 토큰** — 워크스페이스를 *처음 만드는 한 사람*만 쓰는 1회용 값

---

## 1. 가장 빠른 방법 (한 명령)

서버를 돌릴 PC에서:

```bash
# 로컬 테스트(같은 PC에서 앱도 실행):
sb-server --init

# 여러 대에서 쓰기(서버 PC의 사내망 IP 사용):
sb-server --init --bind 192.168.0.10:45871
```

출력 예:

```
✅ 서버 설정 완료 → server.toml

① 이 서버 실행:
     sb-server --config server.toml

② 워크스페이스를 처음 만들 사람(창립자)에게 전달할 setup 토큰:
     82TE82VQS591

③ 참여할 모든 멤버가 앱에 입력할 값:
     서버 주소 : 192.168.0.10:45871
     서버 지문 : 6b2a62c4b8a8...189f1
```

그다음 서버를 켭니다:

```bash
sb-server --config server.toml
```

> `--init`은 `server.toml`(설정)과 서버 신원(`data_dir/identity.bin`)을 만듭니다. 지문은 이
> 신원에서 나오므로 **한 번 만들면 계속 같은 값**입니다. `data_dir`을 지우면 지문이 바뀌니 주의.

빌드가 안 돼 있으면: `cargo build -p sb-server --release` → 바이너리는 `target/release/sb-server`.

---

## 2. 앱에서 접속

앱을 켜면 온보딩 화면이 나옵니다.

**워크스페이스를 처음 만드는 사람 (1명)** — "워크스페이스 만들기" 탭:
- 서버 주소 : 위 ③의 주소
- 서버 지문 : 위 ③의 지문
- 워크스페이스 이름 : 예) `디자인팀`
- setup 토큰 : 위 ②의 토큰
- → "만들기"

**나머지 멤버** — "참여하기" 탭:
- 서버 주소 / 서버 지문 : 위 ③의 값(동일)
- 초대 코드 : *기존 멤버가 앱에서 발급한 코드* (setup 토큰이 아님)
- → "참여"

> 초대 코드는 이미 참여한 멤버가 앱의 **멤버 탭 → 초대 코드 생성**으로 발급해 사내 메신저 등으로
> 전달합니다. 1시간·1회용입니다.

---

## 3. server.toml 설명

```toml
bind_addr = "192.168.0.10:45871"   # 서버가 열 주소. 반드시 사내망(LAN) 주소.
data_dir = "./sb-server-data"       # 신원·로그·초대 저장 위치. 백업 대상(§7.4).
setup_token_hash = "d8be07..."      # setup 토큰의 해시. 서버는 평문 토큰을 보관하지 않음.
```

- `bind_addr`의 IP는 **사설망 대역만** 허용됩니다(`127.0.0.1`, `10.x`, `172.16~31.x`, `192.168.x`
  등). 공인 IP를 넣으면 기동을 거부합니다 — 외부 유출 방지 설계(§4.5).
- 여러 대에서 접속하려면 `127.0.0.1`이 아니라 서버 PC의 실제 LAN IP를 써야 합니다.
  - macOS: `ipconfig getifaddr en0` / Linux: `ip -4 addr` / Windows: `ipconfig`

---

## 4. 항상 켜두기 (선택)

**Linux (systemd)** — `/etc/systemd/system/shareboard.service`:

```ini
[Unit]
Description=shareboard relay
After=network.target

[Service]
ExecStart=/usr/local/bin/sb-server --config /etc/shareboard/server.toml
Restart=on-failure
# 커널 수준 이중 잠금(사내망만):
IPAddressAllow=192.168.0.0/16 127.0.0.0/8
IPAddressDeny=any

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now shareboard
```

**Docker** — 서버 바이너리(musl 정적 빌드 권장)를 담아 `--network host` 로 실행하고
`server.toml`·`data_dir`을 볼륨 마운트.

---

## 5. 잘 안 될 때

| 증상 | 확인 |
|---|---|
| 서버가 `bind_addr ... LAN allowlist 밖` 오류로 안 뜸 | 공인 IP를 넣었음 → 사설망 IP로 변경 |
| 앱이 연결 안 됨 | 방화벽에서 **서버 포트(기본 45871/TCP)** 인바운드 허용했는지 |
| 앱이 "지문 불일치" | 앱에 넣은 지문이 서버 지문과 정확히 같은지(서버 재초기화로 바뀌었을 수 있음) |
| 창립자 "만들기" 실패 | setup 토큰이 서버의 `setup_token_hash`와 짝이 맞는지(다시 `--init` 했다면 토큰도 새 것) |
| 다른 지사(서브넷 분리) | 같은 브로드캐스트 도메인/라우팅 필요. 서버는 한 대만. |

방화벽은 **서버 PC의 인바운드 TCP 1포트**만 열면 됩니다(클라이언트는 인바운드 리스너 없음).
