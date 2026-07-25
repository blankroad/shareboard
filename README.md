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

# 서버 실행
cargo run -p sb-server -- --gen-token          # setup 토큰/해시 생성
cargo run -p sb-server -- --config server.toml # 서버 기동 (지문 출력됨)

# 데스크톱 앱 (Tauri CLI 필요: cargo install tauri-cli --version '^2')
pnpm install
cargo tauri dev      # 개발 실행
cargo tauri build    # 배포 번들
```

### server.toml 예시

```toml
bind_addr = "192.168.1.10:45871"       # LAN 주소여야 함
data_dir  = "/var/lib/shareboard"
setup_token_hash = "<sb-server --gen-token 으로 생성한 hex>"
```

## 아이콘

`assets/icons/app-icon.svg`(마스터) → `scripts/gen-icons.sh` 로 플랫폼/트레이 아이콘 생성
(resvg + `cargo tauri icon`). 트레이는 macOS template image(단색+알파).

## 보안 요약 (PLAN.md §4)

- 전 구간 TLS 1.3 + 콘텐츠는 추가로 그룹 키 E2E. 서버는 blind relay.
- 페어링된(로그에 등록된) 멤버만 통신. 미등록 cert는 Guest lane으로 제한.
- LAN 인터페이스에만 바인딩(사설망 주소 allowlist), 외부 통신·텔레메트리 0.
- 키는 OS keychain(폴백 사다리), 히스토리는 필드 암호화 + crypto-erase.
- 퇴사자 제거 = epoch 키 회전 강제 → 제거된 기기는 이후 복호 불가.

## 현재 상태

코어 6개 크레이트는 완성·테스트됨(95 tests). `sb-server` 통합 테스트가 **2대의 클라이언트가
릴레이를 거쳐 E2E 동기화**되는 전체 흐름을 검증한다. 데스크톱 앱은 코어를 결선해 컴파일되며,
실제 GUI 실행은 `cargo tauri dev`(Tauri CLI)로 한다.

미완/후속 과제: macOS `changeCount` 저비용 감지(현재 폴링), concealed 힌트 플랫폼별 감지,
Wayland 직접 백엔드, GK wrap의 전체 7-필드 AAD·서명 검증(현재 앱은 단순화), 3플랫폼 패키징/서명.
