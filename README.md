# shareboard

사내망(LAN) 전용 크로스 플랫폼(Linux/macOS/Windows) 클립보드 동기화. 서버 중계 + **E2E 그룹 키** 암호화 — 서버는 암호문만 보는 blind relay다. 설계 근거는 [`PLAN.md`](./PLAN.md) (v2.0).

## 아키텍처

```
클라이언트 A ─┐   TLS 1.3 mTLS + 서버 지문 pinning
클라이언트 B ─┼──▶  sb-server (blind relay) ── 암호문·서명본만 중계
클라이언트 C ─┘     그룹 키·평문·초대 코드 미보유
```

- 콘텐츠·정렬키·kind는 전부 그룹 키(GK)로 암호화 → 서버 침해 시에도 내용 비노출.
- 멤버십 = 멤버 서명 **해시체인 워크스페이스 로그**(서버는 append 직렬화만).
- 조인 = 1회용 초대 코드(60-bit + Argon2id). GK는 기존 멤버가 새 기기 공개키로 wrap해 전달.
- 클립보드 변경 시에만 신호(signal)를 보내고 필요한 멤버만 콘텐츠를 fetch.

## 크레이트 구성

| 크레이트 | 역할 |
|---|---|
| `sb-proto` | 와이어 메시지·E2E payload·로그 엔트리·LAN allowlist·파라미터 |
| `sb-crypto` | 신원(P-256+X25519)·그룹키(XChaCha20)·초대(Argon2id)·GK wrap·로그 해시체인 검증 |
| `sb-core` | LWW 판정·에코 방지·E2E 봉인/개봉·설정·히스토리 모델 |
| `sb-net` | TLS 1.3 mTLS 클라이언트 + 서버 지문 pinning + 프레이밍 |
| `sb-server` | blind relay 서버 바이너리 |
| `sb-store` | 암호화 히스토리(rusqlite 필드 암호화)·키 저장소·MAC |
| `sb-clipboard` | 클립보드 접근 트레이트 + mock + arboard 실백엔드 |
| `src-tauri` | Tauri 2 데스크톱 앱(트레이 상주) + Svelte 5 UI |

## 빌드 & 테스트

```bash
# 코어 크레이트 테스트 (95개)
cargo test --workspace

# 서버 실행 (설정 방법: docs/SERVER.md)
cargo run -p sb-server -- --init               # server.toml + 토큰 + 지문 한 번에 생성
cargo run -p sb-server -- --config server.toml # 서버 기동
# 여러 대: cargo run -p sb-server -- --init --bind 192.168.0.10:45871

# 데스크톱 앱 (Tauri CLI 필요: cargo install tauri-cli --version '^2')
pnpm install
cargo tauri dev      # 개발 실행
cargo tauri build    # 배포 번들
```

서버 설정·앱 연결·systemd 등록·문제 해결은 **[docs/SERVER.md](docs/SERVER.md)** 참고.

## 아이콘

`assets/icons/app-icon.svg`(마스터) → `scripts/gen-icons.sh` 로 플랫폼/트레이 아이콘 생성
(resvg + `cargo tauri icon`). 트레이는 macOS template image(단색+알파).

## 보안 요약 (PLAN.md §4)

- 전 구간 TLS 1.3 + 콘텐츠는 추가로 그룹 키 E2E. 서버는 blind relay.
- 페어링된(로그에 등록된) 멤버만 통신. 미등록 cert는 Guest lane으로 제한.
- LAN 인터페이스에만 바인딩(사설망 주소 allowlist), 외부 통신·텔레메트리 0.
- 키는 OS keychain(폴백 사다리), 히스토리는 필드 암호화 + crypto-erase.
- 퇴사자 제거 = epoch 키 회전 강제 → 제거된 기기는 이후 복호 불가.

## CI / 공급망

`.github/workflows/build.yml` — 3-OS 매트릭스(test/fmt/clippy) + 서버 빌드(ubuntu:22.04 컨테이너로
glibc 하한 고정) + 프런트엔드 빌드 + 데스크톱 앱 컴파일 체크 + `cargo-deny`(라이선스·advisory).
`deny.toml` 의 allow 목록에 GPL 계열이 없어 GPL 크레이트는 자동 거부된다(§D20).

## 현재 상태

코어 6개 크레이트 완성·테스트됨(**99 tests**). `sb-server` 통합 테스트가 **2대의 클라이언트가
릴레이를 거쳐 E2E 동기화**되는 전체 흐름을 검증한다. 데스크톱 앱은 코어를 결선해 컴파일되며,
실제 GUI 실행은 `cargo tauri dev`(Tauri CLI)로 한다.

**완료된 후속 과제**
- macOS `changeCount` 저비용 감지(D8) + concealed 힌트(`org.nspasteboard.ConcealedType`) 감지 — `sb-clipboard::macos`.
- GK wrap **서명 + AAD 바인딩 + verify_rotation**(roster·서명·epoch·member_set_hash) — 앱이 검증된 경로 사용.
- CI 워크플로우 + `cargo-deny` 공급망 게이트 + rustfmt.
- Wayland data-control 백엔드 **스캐폴드**(`wayland-backend` feature, Linux 전용·기본 off).
  Docker(ubuntu:24.04, wl-clipboard-rs 0.9.3)로 **컴파일 검증 완료** + Linux 코어 99 tests 통과. CI에 Wayland 컴파일 잡 추가. `scripts/linux-verify.sh` 로 재현.

**남은 후속 과제**
- Wayland 네이티브 **이벤트 감시**(data-control `selection` 구독) — 현재 폴링. Linux 실기기 검증 필요.
- Linux/Windows concealed 힌트 감지(macOS만 구현됨).
- 3플랫폼 실기기 패키징·코드사이닝(`cargo tauri build` + 각 OS 서명 인증서).
