# shareboard 구현 계획서 (v2.0 — 서버 중계 + E2E 그룹 키)

수석 아키텍트 통합안 — v1.1(full-mesh P2P) 위에 5개 축 재설계(group-crypto / server-architecture / protocol-v2 / ui-flows-v2 / milestones-v2)를 통합. 축 간 모순은 §2 하단 "조정 사항"에서 근거와 함께 단일 결정으로 확정함.

## v1.1 → v2.0 주요 변경

- **토폴로지 전환(사용자 결정)**: full-mesh P2P + 페어와이즈 SPAKE2 페어링을 폐기하고, 사내 상시 서버 1대에 전 클라이언트가 접속하는 **hub-and-spoke**로 전환. 서버가 접속·중계·멤버십 admission을 담당.
- **E2E 유지**: 콘텐츠는 클라이언트들만 아는 **그룹 키(GK)**로 암호화 — 서버는 암호문만 보는 **blind relay**. 서버가 침해되어도 콘텐츠 노출 불가가 설계 목표(D25·D26).
- **조인 = 1회용 초대 코드**: 12자(60-bit) 코드 + 위임형 봉인 초대(blob에 GK 미포함, D27) + 멤버 서명 해시체인 워크스페이스 로그(D28). 미감사 spake2 의존 제거(D3 폐기).
- **신규 산출물**: 같은 cargo workspace의 `sb-server`(Rust 단일 바이너리, headless, systemd/Docker, LAN 전용) + 공유 프로토콜 크레이트 `sb-proto`. 클라이언트 인바운드 리스너·mDNS 상호 탐색 폐지.
- **유지**: Rust+Tauri 2 클라이언트, 플랫폼별 클립보드 감지 전략(§6), 텍스트+PNG, 히스토리(§7), concealed 필터, UI 프레임(Svelte 5)·아이콘(§9), 로깅 원칙(§4.7), LAN 전용·외부 통신 0·텔레메트리 0.

## 검증 과정에서 보강된 사항

- **인증 없는 서버 통지의 권위 격하**: `Revoked`/`Bye{Revoked}`는 힌트로만 처리 — 로컬 키 파기·히스토리 crypto-erase는 **검증된 Remove(self) 엔트리 + 명시적 사용자 확인** 이중 게이트에서만(§5.4). 로그 검증 규칙에 Epoch `member_set_hash`/`wrapped_bundle_hash` 재계산 대조 추가(§4.3.1).
- **부트스트랩·복구 강화**: setup 토큰은 관리자 오프라인 생성 후 `server.toml`에 **해시로 주입**(침해 서버가 평문 토큰 미열람), 조인 UI가 서버 지문+**창립자 지문**을 out-of-band 대조(§4.3.2). orphaned/마지막-멤버 벽돌화 대비 `sb-server reclaim` + 백업/이전 플레이북 신설(§7.4·§11 M6).
- **Guest lane·로그 오염 방어**: `AppendEntry(Add)`를 **미소비 초대 locator에 바인딩**(초대당 Add 1건), 로그 총량 상한·체크포인트, 포크/스킵 회복 규칙 명시(§4.3.1·§4.5).
- **split-view·회전 정합**: presence head-hash 교차검증 **필수화**, `enc_profile`에 단조 seq 바인딩(재생 거부), RotationBlob를 채택된 Epoch 엔트리 해시에 바인딩(§4.3.1·§4.4).
- **현실 정정**: §6 GNOME은 data-control **불가군**으로 재분류(폴백 경로 1급 문서화), M3를 M3a/M3b로 분할·단일 개발자 일정 30~45주로 재기준(§6·§11), fetch를 cache-through 후 fan-out으로 재정의(§5.4).

---

## 1. 개요와 목표

**shareboard**는 사내망(LAN) 전용 크로스 플랫폼(Linux/macOS/Windows) 클립보드 동기화 앱이다. 시스템 트레이 상주형 클라이언트와, 사내 상시 호스트에서 도는 경량 중계 서버 **`sb-server`**로 구성된다. 전원이 하나의 클립보드 그룹(단일 보드)을 공유하며, UTF-8 텍스트와 PNG 이미지를 **그룹 키 E2E 암호화**로 동기화한다. 목표 규모 N≤10(서버 상한은 여유 있게 64 연결).

**목표**
- 클립보드 변경을 OS 이벤트 기반으로 감지, 변경 시에만 서버로 신호(signal)를 보내고 필요한 멤버만 콘텐츠를 가져가는(fetch) 저대역폭 동기화. 서버는 signal 팬아웃 + head 캐시(최신 1건 수렴)만 담당.
- 콘텐츠·정렬키·kind·장치명은 전부 그룹 키 암호문 — **서버가 생성·해석하는 보안 데이터 0**(blind relay). 서버 완전 침해 시에도 콘텐츠 비노출.
- 멤버 admission = 워크스페이스 로그(멤버 서명 해시체인), 조인 = 1회용 초대 코드. 코드가 그룹 키의 E2E 전달을 암호학적으로 보증하며 서버는 코드·GK를 알 수 없음.
- 서버: Rust 단일 바이너리, headless, **설정 파일 하나(server.toml)**, LAN 인터페이스에만 바인딩, 사내망 외부 통신 0, systemd/Docker 배포, 운영 부담 최소.
- 유휴 CPU ~0%, 클라 RSS<80MB, 서버 유휴 RSS<30MB. 창 미표시 시 WebView 미상주.
- 멤버 관리 / 히스토리 / 동기화 on-off / 설정 UI + 진단 패널 + 앱·트레이 아이콘.

**비목표 (명시적 범위 밖)**
- 모바일, 파일 전송, 클립보드 클리어 동기화, FTS, 인터넷 경유/NAT traversal(iroh류 금지), 자동 업데이트 서버(수동 배포 절차·문서는 M6), 오프라인 기간의 중간 히스토리 재동기화(최신 1건만 수렴), 동일 호스트 악성 프로세스의 OS 클립보드 직접 접근 방어, **서버 이중화/HA**(R-SPOF는 §12에서 수용), 다중 워크스페이스(서버 1대 = 보드 1개).

---

## 2. 아키텍처 결정 요약표

| # | 결정 | 근거 (한 줄) |
|---|---|---|
| D1 (유지) | Rust + Tauri 2 (2.11.x), 단일 프로세스 + tokio task 상주, 데몬 분리 없음 | v1.1 그대로 |
| D2 (개정) | 전송: 클라→서버 **단일 아웃바운드 TCP + rustls 0.23 TLS 1.3 전용**. 서버 = 자기서명 cert + SHA-256(SPKI) 지문 pinning(초대 blob 경유 검증 + TOFU), 클라 = mTLS 장치 cert(로그 admission 지문) | 감사된 rustls 유지; 단방향 연결로 클라 공격면·방화벽 규칙 최소화 |
| D3 (폐기) | SPAKE2 페어와이즈 페어링·spake2 크레이트 **제거** | 페어링 개념 소멸(D27·D28로 대체). v1.1이 명시한 미감사 spake2 잔여 리스크 통째 소거 |
| D4 (개정) | 장치 identity = ECDSA P-256(rcgen) 1개로 **TLS·로그 서명 겸용**(domain-sep context "sb/log/v1"), device_id = SHA-256(SPKI). **X25519 KEM 키 신설**(GK wrap 수신용) | 단일 신원 모델 유지 + 키 용도 분리 |
| D5 (유지) | serde + ciborium(CBOR), bincode 금지 | v1.1 그대로 |
| D6 (폐기) | 클라이언트 간 mDNS 상호 탐색(browse+register) 폐기. 서버 자기 광고(`_shareboard-srv._tcp`)만 opt-in(기본 off) | 발견 문제가 "서버 주소 1개 도달"로 축소 — R4 사실상 소멸(§12) |
| D7 (유지) | clipboard-rs 0.3 + wayland-client 직접 구현(wl-clipboard-rs 개작) + arboard 폴백 | v1.1 그대로 |
| D8 (유지) | macOS `NSPasteboard.changeCount` 정수 비교 워칭 | v1.1 그대로 |
| D9 (폐기) | full-mesh·"릴레이 금지" 폐기 → **서버가 유일 릴레이, 클라이언트는 수신 콘텐츠 재발행 금지** | 단일 허브 경로라 다단 루프 구조적 불가는 동일하게 성립(§5.3) |
| D10 (개정) | LWW 판정식 `(lamport, wall_ts, device_id)` 유지. 단 **정렬키는 E2E 암호문 내부** — 서버는 순서 미판정, 클라가 복호 후 판정 | 서버(침해 포함)의 재정렬 조작 원천 차단 + 메타데이터 최소화(D31) |
| D11~D18 (유지) | 저장(rusqlite+필드 암호화) / keyring 사다리 / 히스토리 opt-in / Svelte 5 / PNG 패스스루·zstd 텍스트 / Windows 제외 포맷 / concealed 필터 / 아이콘 C안 | v1.1 그대로 |
| D19 (개정) | TCP 45871 = **서버** 리슨 포트(server.toml). 클라이언트 리스너·`listen_port` 설정 폐지 — 아웃바운드 1연결, 인바운드 0 | 방화벽 = 서버 호스트 인바운드 1규칙로 축소, 클라 공격면 소멸 |
| D20 (유지) | AGPL 복사 금지·GPL 회피, MIT/Apache-2.0 개작 허용 | v1.1 그대로 |
| D21 (개정) | keyed 해시 원칙을 와이어로 확장: **ContentId = keyed BLAKE3(k_cid[epoch], plaintext)** — 서버·와이어·디스크 어디에도 unkeyed 평문 해시 노출 금지. DB dedup keyed 해시는 기존 유지 | unkeyed 해시를 서버에 주면 저엔트로피 원문 사전 대입 역산 표면 — D21의 정신을 와이어로 확장 |
| D22 (개정) | 전용 MAC 키 역할 전환: peers.json → **workspace.json**(로그 캐시·server fp·last_head·epoch 단조성 기록) 무결성 보호. HMAC-SHA256, keychain 상시 provisioning | 로컬 캐시 변조·로그 롤백 유도 차단 |
| D23 (유지) | tracing 로깅 + 민감정보 금지 — **서버에도 동일 원칙 적용** | v1.1 그대로 + 서버 확장 |
| D24 (개정) | wake/네트워크 변경 감지 유지, 용도 축소: **재연결 백오프 리셋 전용**(리스너 재바인딩·mDNS 재등록 소멸) | 재연결 대상이 고정 서버 1개 |
| D25 (신규) | **hub-and-spoke**: `sb-server` — 같은 workspace의 Rust 단일 바이너리, headless, blind relay(암호문·서명본만 취급), systemd/Docker, LAN 바인딩, 외부 통신 0 | 사용자 결정(토폴로지 전환) + "설정 파일 하나 수준" 운영 목표 |
| D26 (신규) | **E2E 그룹 키 GK_e**: 256-bit CSPRNG, XChaCha20-Poly1305, epoch 단조 회전. revoke 시 회전 **필수·원자 플로우**(회전 없는 제거 동작 자체가 없음) | 실질 차단 근거 = "GK_{e+1}을 모름"(서버 차단은 심층 방어일 뿐) |
| D27 (신규) | 조인 = **위임형 봉인 초대(GK 미포함)**: 60-bit 코드(Crockford Base32 12자) + Argon2id(64MiB) → locator/K_seal 파생, blob은 grant_sk(일회용 admission 권한)만 봉인. GK는 조인 확정 후 기존 멤버가 새 기기 X25519 pk로 wrap 전달 | 코드 대입 가치가 TTL로 시간 제한, 서버는 어떤 시점에도 저엔트로피 보호 하의 GK 미보유(§4.3) |
| D28 (신규) | roster 무결성 = **워크스페이스 로그**(멤버 서명 해시체인: Genesis/Add/Remove/Epoch). 서버는 append 직렬화만 — 어떤 엔트리도 위조 불가 | 키 회전 시 wrap 대상이 roster에서 도출되므로 roster 무결성이 정확히 load-bearing(§4.3.1) |
| D29 (신규) | 서버 head 캐시 = **메모리 전용**(디스크 영속 금지) | 미래 GK 유출 시 소급 노출 표면 축소 + 보존/삭제 정책 관리 부담 0; 재시작 복구는 재발행으로 해결(§5.4) |
| D30 (신규) | 서버 상태 = 파일 저장(로그 append 파일 + state.json atomic write), SQLite 미도입 | 멤버 ≤64·저빈도 쓰기에 마이그레이션/의존 부담이 손해 |
| D31 (신규) | 평문 와이어 헤더 최소화: `SignalHdr = {id, epoch, ct_size}`만. kind·lamport·wall_ts·origin·inline 여부는 전부 E2E 내부 | 서버 학습 가능 메타데이터 최소화(§4.1 잔여 위험 표 참조) |

**재설계 축 간 조정 사항 (모순 해결 기록)**

1. **조인 프로토콜**: server-architecture·protocol-v2의 "서버 릴레이드 SPAKE2(발급자 동시 온라인 필수)" vs group-crypto의 "위임형 봉인 초대(b′)" → **b′ 채택(D27)**. 근거: 미감사 spake2 소거, 서버에 PAKE 릴레이 랑데부 서브시스템 불요(서버 최소화 목표 부합), 발급자 동시 온라인 불요(GK 전달만 "임의 멤버 1명 온라인" 필요), roster 무결성이 서명 체인(D28)으로 함께 해결. 초대 TTL·코드 길이도 b′ 기준(기본 1h·최대 24h, 12자/60-bit — §4.3.6 엔트로피 분석).
2. **ContentId**: `blake3(ciphertext)`안 vs `keyed BLAKE3(plaintext)`안 → **keyed BLAKE3(plaintext)** (D21 개정). 근거: 멤버 간 dedup/suppress 의미론 보존(암호문 해시는 nonce 때문에 매 발행 상이), 서버 역산 불가. 전송 무결성은 TLS + AEAD 태그 + 수신 후 CID 재계산이 전담.
3. **평문 헤더 범위**: group-crypto의 평문 lamport/wall_ts(서버가 LWW 비교) vs protocol-v2의 E2E 내부화 → **E2E 내부화(D31)**. 근거: 메타데이터 최소화 우선, AAD 결합의 변조 방어 효과는 동일, 서버 측 순서 판정은 "head 최근 K건 캐시 + 클라 판정"으로 대체.
4. **재연결 백오프 상한**: 60s vs 30s → **30s**(대상이 상시 서버 1대 — 빠른 복구 이득 > 부하 우려. ±20% jitter·D24 리셋은 유지).
5. **head 캐시 구성**: "최신 1건" vs "signal 4건+본문 1건" → **signal 4건 + 본문 최신 1건**(서버가 순서를 모르므로(D31) 직전 K건을 넘겨 클라가 LWW 판정 — 3항의 귀결).
6. **부트스트랩**: "최초 Genesis 선착" vs "setup 토큰" → **setup 토큰 병용, 단 토큰은 관리자 오프라인 생성·서버엔 해시만 주입**. 암호학적 신뢰 근거는 Genesis 서명 체인이고, 토큰은 신설 서버의 클레임 경쟁을 막는 admission 편의다. 침해된 부트스트랩 서버가 평문 토큰을 읽어 정당 창립자보다 먼저 `ClaimWorkspace`하는 것을 막기 위해, 서버는 토큰을 **생성·출력하지 않고** `server.toml`의 `setup_token_hash`(관리자가 오프라인 생성한 토큰의 SHA-256)만 보유하며 클레임 시 제시 토큰의 해시 일치로 검증한다(재발급 = 관리자가 새 토큰·해시 교체). 추가로 조인자는 서버 지문뿐 아니라 **창립자 지문**(Genesis `creator_spki` SHA-256)을 관리자 공지값과 out-of-band 대조하여 "정당 창립을 가장한 침해 서버"(§4.1) 경로를 차단한다.

---

## 3. 시스템 아키텍처

### 3.1 클라이언트 프로세스/모듈 구성

```
┌─ Tauri main process ─────────────────────────────────────────────┐
│  setup()에서 tauri::async_runtime(tokio 공유)으로 spawn:          │
│   ├─ clipboard watcher task   (OS 이벤트 → mpsc)                 │
│   ├─ server session task      (서버 1연결: TLS·Hello·재연결 FSM) │
│   ├─ sync engine task         (signal→fetch·LWW·suppress)        │
│   ├─ group task               (로그 체인 검증·GK/epoch·wrap 처리) │
│   ├─ power/net watch task     (D24: wake·변경 → 백오프 리셋)      │
│   └─ store task               (히스토리/설정/workspace 캐시 영속) │
│  상태 공유: tauri::State<AppCore> — watch(스냅샷) + mpsc(작업)    │
├─ WebView (창이 열릴 때만 생성, close 시 destroy) ────────────────┤
│  Svelte 5 SPA — invoke(command) / listen(event)만 사용            │
└──────────────────────────────────────────────────────────────────┘
```

- v1.1 대비: mdns task·net listener task·피어별 세션 task **소멸** → 서버 세션 task 1개 + group task 신설. 코어 로직은 `AppHandle` 비의존 순수 crate(sb-*) 격리·채널 통신·headless 테스트 원칙 그대로.

### 3.2 sb-server 모듈 구성 (신규)

같은 cargo workspace의 신규 멤버 2개:

```
src-tauri/crates/
├─ sb-proto/          # [신규·공유] 와이어 타입(C2s/S2c, SignalHdr, LogEntry), ciborium 직렬화,
│                     #   LengthDelimitedCodec 프레이밍 상수, 주소 allowlist(§4.5 — sb-net에서 이관)
└─ sb-server/         # [신규] headless 단일 바이너리
   ├─ main.rs         #   CLI: run | fingerprint | healthcheck | verify-log | status | reclaim (수동 파싱)
   │                  #     verify-log=체인·torn-write 검사, status=claimed·seq·head·연결 멤버,
   │                  #     reclaim=claimed 해제+새 setup_token_hash 유도(명시 확인·백업, §7.4)
   │                  #     ※ setup 토큰은 서버가 생성·출력하지 않음 — 관리자 오프라인 생성·해시 주입(§2 조정6)
   ├─ config.rs       #   server.toml 로드·검증(bind 주소 allowlist 강제, 0.0.0.0/:: 거부)
   ├─ identity.rs     #   rcgen self-signed cert(P-256) 생성/로드, 파일 0600, fp = SHA-256(SPKI)
   ├─ tls.rs          #   rustls ServerConfig(TLS 1.3 전용), ClientCertVerifier:
   │                  #     로그 admission 지문 → Member lane / 미지 cert → Guest lane 태깅(거부 아님)
   ├─ listener.rs     #   allowlist 인터페이스 bind, accept, 연결 수·IP별 token bucket
   ├─ session.rs      #   연결 FSM: Guest | Member(Hello→Ready), 버전 협상
   ├─ router.rs       #   SignalFanout(발신자 제외), presence 브로드캐스트
   ├─ head.rs         #   head 캐시(메모리 전용, D29): signal 4건 + 본문 1건, 청크 릴레이/티잉
   ├─ wslog.rs        #   워크스페이스 로그 append-only(길이-프리픽스+엔트리 해시로 torn-write 감지,
   │                  #     기동 시 tail 검증·마지막 온전 엔트리까지 트렁케이트 복구), 직렬화·admission
   │                  #     지문 도출, setup_token_hash 검증, Add 미소비 초대 locator 바인딩(§4.5)
   ├─ invite.rs       #   초대 blob 저장소(locator→blob, TTL, consumed 편의 플래그)
   ├─ mailbox.rs      #   KeyUpdate 보관함(멤버별 최신 1개)
   ├─ health.rs       #   127.0.0.1 HTTP GET /healthz (수제 초소형 응답, 프레임워크 미도입)
   └─ limits.rs       #   상한 상수·rate limit 집약
```

- tokio: multi_thread, `worker_threads = 2`. 연결당 read/write task, bounded mpsc로 backpressure.
- 로깅: tracing + §4.7 원칙 동일. 초대 blob·wrap payload 로깅 금지, fp는 앞 8바이트 표기. 기본 stdout(journald/Docker 수집), `log_dir` 설정 시 파일 회전(7일).
- 서버는 로그 엔트리를 구조적으로 읽어(admission 지문·epoch 번호 도출) 접속 허가·mailbox 라우팅에 쓰지만, **어떤 엔트리도 서명 불가 = 위조 불가**(D28). 보안 판정의 권위는 항상 클라이언트 측 체인 검증.

### 3.3 데이터 흐름

**송신 경로 (클라)**

```
OS 클립보드 이벤트
 → 150ms debounce → [사전 판별] 포맷/크기 조회 — READ_HARD_LIMIT 32MiB 초과 시 read 중단·스킵
 → suppress set 매칭? → yes: 소비(에코 차단)
 → [동일 클립보드 세션] concealed 마커 확정 → 콘텐츠 read (마커 존재 시 즉시 폐기·zeroize, §4.6)
 → ContentId = keyed BLAKE3(k_cid[epoch], plaintext) → lamport += 1
 → 인메모리 히스토리 + 송신 캐시(5개/5분) [+opt-in 암호화 디스크 저장, keyed dedup D21]
 → size > 10MiB? → 로컬 히스토리만, 전파 안 함
 → (>4KiB 텍스트 zstd — 암호화 전) → SignalBody{kind, lamport, wall_ts, origin, inline?}를 GK_e로 봉인
 → ClipSignal{SignalHdr, e2e} 서버 업로드 → 서버가 전 멤버 팬아웃(발신자 제외)
```

**수신 경로 (클라)**

```
SignalFanout 수신 → AEAD open(GK_e, AAD = SignalHdr‖origin) — 실패 = 변조/구 epoch → 폐기
 → LWW key 비교(복호한 lamport/wall_ts/origin) / suppress 검사 → 패배·중복이면 폐기
 → inline 있으면 즉시 적용; 없으면 ContentRequest → 서버(캐시 or origin 릴레이) → 청크 수신
 → AEAD open + keyed CID 재계산 일치 검증(불일치 1회 재요청)
 → [고위험 패턴 검사] 자동 적용 대신 알림 → 클릭 시 적용 (confirm_risky_content, v1.1 유지)
 → suppress set 등록 → OS 클립보드 set (Windows 제외 포맷 동시 기록)
 → current_applied 갱신, lamport merge, 히스토리 기록, UI 이벤트 emit
```

### 3.4 IPC 경계 (command/event 전면 개정)

**UI→Rust command**

| 분류 | Command | 비고 |
|---|---|---|
| 온보딩 | `get_onboarding_state` | `unconfigured \| complete` — 창 오픈 시 모드 분기 |
| | `test_server_connection(addr)` | 단계별 결과 + 서버 지문·버전 반환. 진단 패널과 공용 |
| | `parse_invite_uri(uri)` → `{addr, fingerprint?, code}` | URI 파싱은 Rust에서(프론트 정규식 금지) |
| | `create_workspace{server_addr, server_fp, setup_token, workspace_name, device_name?}` | Genesis 서명·업로드, GK 로컬 생성 |
| | `join_workspace{server_addr, server_fp?, invite_code}` / `cancel_join` | 진행은 `join-progress` 이벤트. **기존 워크스페이스 소속 감지 시 명시 확인 후 leave(crypto-erase) 선행 강제** — 무단 덮어쓰기로 구 GK·캐시 잔류 금지(§8.2) |
| 초대 | `create_invite(ttl_s?)` → `{code, uri, expires_at}` | 발급 기기당 활성 1개 |
| | `cancel_invite` / `get_active_invite` | 철회 = 서버 blob 삭제 |
| 멤버 | `get_members` / `revoke_member(device_id)` | revoke = 키 회전 동반(원자) |
| | `get_my_device` / `rename_device(name)` | 이름은 E2E profile로 전파 |
| 키 | `rotate_group_key` | 수동 회전(D26 트리거) |
| 서버 | `get_server_status` / `reconnect_now` | 배너 [지금 재시도] |
| 동기화 | `set_sync_enabled(bool)` / `pause_incognito(minutes)` | 유지 |
| | `apply_pending_content(id)` / `dismiss_pending_content(id)` | 고위험 확인 흐름(유지) |
| 히스토리 | `get_history` / `copy_history_item(id)` / `delete_history_item(id)` / `clear_history` | 유지 |
| 설정 | `get_settings` / `update_settings` | 유지 |
| 진단 | `get_app_info`(앱·프로토콜·서버 버전) / `export_diagnostics` | 유지·확장 |
| 위험 | `leave_workspace` / `reset_app` | crypto-erase 동반 |

삭제된 command: `get_peers`, `start_pairing`, `submit_pairing_code`, `approve_peer`, `reject_peer`, `unpair_peer`, `add_manual_peer`, `run_connection_test`(→ `test_server_connection`).

**Rust→UI event**

| Event | Payload 요지 |
|---|---|
| `server-status-changed` | `{state: connected\|reconnecting\|unconfigured, addr?, next_retry_ms?, reason?}` |
| `members-updated` | 멤버 목록 전체 스냅샷(이름/플랫폼/온라인/last_seen/epoch 수신 여부) |
| `member-joined` / `member-revoked` | 알림·배너용 `{name, platform, invited_by?}` |
| `invite-state-changed` | `{state: active\|used\|expired\|cancelled, used_by?}` |
| `join-progress` | `{step: tcp\|tls\|invite_blob\|log_verify\|register\|group_key, status: running\|ok\|failed\|waiting, reason?}` |
| `key-rotation` | `{phase: started\|progress\|completed, updated, total, epoch}` |
| `clipboard-synced` / `clipboard-pending-confirm` / `sync-state-changed` / `history-updated` / `error` | v1.1 유지 |

삭제된 event: `peer-discovered`, `peer-online/offline`, `peer-conn-failed`, `pairing-started`, `pairing-code`, `pairing-result`.

- 대용량 payload emit 금지 — 썸네일은 `tauri::ipc::Response` 바이너리, 평문 전문은 WebView에 미전달(§4.6). v1.1 원칙 그대로.

---

## 4. 보안 설계

### 4.1 위협 모델 — "반신뢰 서버"(honest-but-curious + 침해 가능) 중심 재작성

| 위협 | 대응 | 잔여 위험 |
|---|---|---|
| 서버의 콘텐츠 열람 | E2E XChaCha20-Poly1305(GK는 클라이언트만 보유), kind·inline 여부도 암호문 내부(D31), ContentId는 keyed(사전 대입 역산 불가 — D21) | 크기·빈도·타이밍·송신자·fetch 패턴 메타데이터, ContentId equality linkage(같은 내용 재복사 연결) — 수용, v1.1 도청 행과 동급 |
| 서버의 변조·재라우팅·재정렬·재전송 | AAD = SignalHdr‖origin 결합 + 정렬키가 AEAD 내부(D10 개정) → 변조 시 전 수신자 복호 실패. 원본 그대로 replay는 LWW/dedup/suppress가 무해화. epoch 격리(revoke 시 grace 0) | 선택적 전달·지연(특정 멤버에게만 미전달) = 가용성 공격 — presence head-hash 교차검증 **필수화**(발산 시 동기화 차단·경고, §4.3.1)로 탐지 강제 (완전 격리 단일 클라 잔여는 아래 행) |
| 서버의 split-view로 revoke 은닉(제거자+격리 클라가 epoch-e "섬" → 제거 멤버가 신 epoch 트래픽 계속 복호) | Remove/Epoch tail 학습 시 신 epoch 차단이 성립. **강화**: presence head-hash 교차검증 필수 + `enc_profile` 봉인 내부에 단조 seq/epoch/ts 포함(서버의 stale profile 재생으로 head 발산 은폐 거부, §5.2) + 서명된 epoch heartbeat(신선도 상한)·다수 멤버 presence 확인 전 "최신 epoch" 신뢰 보류 | 완전 격리된 단일 클라이언트의 지속 split-view는 heartbeat 신선도 상한 시간까지로 **시간 제한**(무기한 아님). 그 창 내 구 epoch 송신 지속은 v1 잔여(신 epoch 콘텐츠는 비노출) |
| 서버의 roster 위조(공격자 기기 주입 → 차기 회전 시 GK 절취) | 멤버 서명 해시체인(D28) — 서버는 Genesis/Add/Remove/Epoch 어느 것도 서명 불가, 회전자의 wrap 대상은 검증된 체인에서만 도출. Epoch `member_set_hash`를 각 멤버가 체인에서 독립 재계산해 대조(§4.3.1 규칙 ⑥) — 회전자가 roster 밖 키를 wrap해도 탐지 | 엔트리 은닉·롤백(split-view): head 단조성(keychain 보관, D22) + presence 교차검증(필수)으로 탐지 |
| 서버의 초대 blob 오프라인 대입/사전계산 | **60-bit 원시 엔트로피 + Argon2id(64MiB) 메모리 경도** → 전수 대입·사전계산 테이블 모두 물리적으로 비현실(§4.3.6 정정 — salt=workspace_id 공개·불변이라 사전계산이 TTL에 안 묶임을 인정하고 근거를 엔트로피로 이관, "TTL이 대입을 제한" 논거 폐기) + **blob에 GK 미포함(D27)** | 코드 자체 유출(어깨너머·스크린샷) 시 TTL 내 위장 조인 → 조인 UI 알림 + 즉시 revoke·필수 회전으로 사후 대응 |
| 초대 1회성 우회 | 체인 규칙 "동일 grant_pk의 Add는 최초 1건만 유효" — 서버 신뢰 불요(consumed 플래그는 편의) | 코드 탈취 + 악의 서버의 공격자 Add 선배치 결합 시나리오 (조인 알림 탐지 + revoke) |
| 서버 완전 침해(과거 트래픽 녹화 포함) | 서버는 암호문·서명본만 보유 — GK 부재로 콘텐츠 소급 복호 불가. 이후 능동 공격은 위 행들로 환원 | **정적 GK의 FS 한계** — 하단 문단 참조 |
| 조인 시 가짜 서버/워크스페이스 유인 | blob AEAD(K_code)가 workspace_id·로그 head·서버 cert 지문을 코드에 바인딩 — 코드 없는 서버는 유효 blob 제조 불가, AEAD 성공이 곧 상호 인증 | 없음 (코드 정직 전달 전제) |
| **부트스트랩 침해 → "정당 창립" 가장**(침해 서버가 평문 setup 토큰 절취 후 먼저 클레임 → 자신이 창립자 되어 GK 보유) | setup 토큰을 서버가 생성·보유하지 않고 `setup_token_hash`만 보유(§2 조정6·§4.3.2) → 서버는 평문 토큰 미열람. 조인 UI가 **창립자 지문(Genesis creator_spki SHA-256)**을 관리자 공지값과 out-of-band 대조 → 공격자 창립 Genesis 거부 | 관리자 공지 지문 채널 자체 침해 (수용, TOFU 동급) |
| Guest lane 메타데이터 노출(임의 LAN 호스트가 GetLog로 roster·workspace_name 열람) | Guest `GetLog`를 **유효 locator 제시(코드 소지 증명) 이후로 게이트**(§4.5) — 코드 없는 호스트는 체인 미열람 | 코드 소지자에게 roster/workspace_name은 공개 메타데이터로 수용(콘텐츠는 불노출) |
| 수동 도청 (LAN sniffing) | 전 구간 TLS 1.3 + 내용은 추가로 E2E | 크기/타이밍 메타데이터 (수용) |
| 침해된 승인 멤버 | GK 보유자이므로 정렬키 위조·임의 콘텐츠 주입 가능 — 고위험 패턴 확인 후 적용(§3.3), 즉시 revoke = 필수 키 회전(D26), 조인/제거 전 멤버 알림 | revoke 이전 유출, 패턴 미검출 콘텐츠 자동 적용 (수용) |
| 디스크 키/히스토리 탈취 | 키는 OS keychain, 히스토리 필드 암호화 + crypto-erase | OS 계정 자체 침해 (수용) |
| 히스토리 DB 해시 역산 | dedup 해시 = keyed BLAKE3(D21) | keyed 키까지 탈취되는 OS 계정 침해 (수용) |
| workspace.json(로컬 캐시) 변조 | 전용 MAC 키(D22 개정) HMAC 검증 — 실패 시 캐시 격리, 서버에서 로그 재취득·전체 재검증 | 파일과 keychain 동시 침해 (수용) |
| 비밀번호 매니저 유출 | concealed 힌트 동일 세션 원자 검사(D17) → 기본 제외+미저장 | 힌트 없는 매니저 → 기본 excluded_apps 동봉 + 정규식 제외 + 최초 실행 고지 |
| DoS | Guest lane 상한(IP당 1·전체 8·frame 32KiB·TTL 60s), IP token bucket, 프레임 256KiB 상한, signal 10/s 서버 강제, fetch 동시 1 | LAN 플러딩 (수용). 서버 다운 = R-SPOF(§12 N1) |
| 다운그레이드 | TLS 1.3 전용, 앱 프로토콜 버전 게이트(서버 수행, 지원 창 = 직전 1개) | 없음 |
| WebView 공격면 / 외부 유출 / 로그 유출 / 공급망 | v1.1 그대로: CSP `connect-src ipc: http://ipc.localhost`·capability 최소 / 아웃바운드는 설정된 서버 주소만(allowlist 검증, §4.5)·텔레메트리 0 / §4.7 금지 규칙 / cargo audit·deny 게이트 | v1.1과 동일 (수용 범위 동일) |

**Forward secrecy 한계 명시와 수용 근거**: 두 개의 독립 FS 축을 모두 명시한다. **(축 A — 정적 GK)** epoch 내 정적 GK이므로 메시지 단위 FS 없음 — GK_e 유출 시 그 epoch에 수집된 암호문 전부가 열린다. **(축 B — 정적 KEM 수신키)** GK wrap이 수신자 정적 X25519 키에 대한 ECDH-ES라, 어느 한 장치의 KEM sk가 미래에 유출되면 서버 mailbox에 녹화된 그 장치 앞 **과거·미래 전 epoch의 wrap을 소급 unwrap**해 해당 epoch GK로 콘텐츠를 복호할 수 있다(축 A보다 넓은 소급 창). 수용 근거: (1) 클립보드는 단명 데이터이고 서버 정상 보존은 head 캐시(메모리, 즉시 교체)뿐이라 소급 노출 창이 구조적으로 작음(D29), (2) 히스토리는 로컬 별도 키로 분리, (3) N≤10 사내망에서 MLS/TreeKEM급 그룹 래칫은 "감사 가능한 소박한 암호 구성 + 서버 관리 최소화" 목표와 상충. **부분 완화**: epoch 회전이 coarse-grained FS/PCS 경계 제공(revoke 시 필수, 수동 버튼, 주기 회전 옵션 30/90일 — 기본 off), 회전 키는 구 키에서 비파생. **축 B 완화(신설)**: 장치 KEM 키를 주기 회전 — 회전 멤버는 서명된 로그 엔트리(`RotateKem{new_kem_pk}`, Add와 동일한 멤버 서명·검증)로 새 KEM pk를 게시하고 구 KEM sk를 즉시 zeroize하여, 미래의 단일 KEM sk 유출이 과거 wrap을 열지 못하게 한다(기본: GK 주기 회전과 연동, 최소 revoke 회전 시 제거자 아닌 잔존 멤버도 차기 조인 전 KEM 갱신 권장). "녹화하는 침해 서버 + 미래 GK/KEM sk 유출" 결합은 잔여 위험으로 문서화하고 배포 문서에 주기 회전 권장 명시.

### 4.2 신원과 키

| 키 | 알고리즘 | 수명 | 저장 |
|---|---|---|---|
| 장치 identity — 대서버 mTLS cert + 로그 서명 겸용(context "sb/log/v1" domain-sep) | ECDSA P-256 (rcgen + p256), device_id = SHA-256(SPKI) | 장치 수명 | OS keychain (keyring v3) |
| 장치 KEM 키 (GK wrap 수신용) — **신규** | X25519 (x25519-dalek 2) | 기본 장치 수명, **주기 회전 지원**(서명된 `RotateKem` 로그 엔트리로 새 pk 게시, 구 sk 즉시 zeroize — §4.1 축 B 완화) | OS keychain |
| **그룹 키 GK_e** — 신규 | 256-bit CSPRNG(OsRng), XChaCha20-Poly1305 | epoch (회전 시 grace 후 zeroize) | OS keychain (현 epoch만) |
| 파생키 k_cid / k_sig / k_body | HKDF-SHA256(GK_e, "sb/cid-v1" 등 — 용도별 도메인 분리로 블롭 교차 스플라이싱 차단) | GK_e와 동일 | 파생 — 미저장 |
| 초대 코드 | 60-bit CSPRNG → Crockford Base32 12자 | TTL 기본 1h·최대 24h, 1회 | 메모리·표시 전용, zeroize |
| K_code → locator / K_seal | Argon2id(salt = workspace_id, m=64MiB, t=3, p=1, ~0.5s) + HKDF 분기("sb/inv-locator"/"sb/inv-seal") | 초대 수명 | 메모리, zeroize |
| grant 키쌍 (초대별 일회용) | ECDSA P-256 | 초대 수명, Add 확정 시 파기 | sk는 초대 blob 내부에만 존재 |
| KeyUpdate/조인 wrap | X25519 ECDH-ES + HKDF-SHA256 + XChaCha20-Poly1305 | 1회성 | 서버 mailbox (암호문) |
| 서버 TLS cert | self-signed(rcgen), SHA-256(SPKI) 지문 pinning(초대 blob 경유 검증 + TOFU) | 서버 수명 | 서버: 파일 0600 / 클라: 지문만 설정에 |
| 워크스페이스 캐시 MAC 키 (D22 개정) | HMAC-SHA256, 256-bit | 장치 수명, 상시 provisioning | OS keychain |
| 히스토리 키 / dedup 해시 키 (D21) | v1.1 그대로 | v1.1 그대로 | OS keychain |
| 세션 키 | TLS 1.3 파생 | 세션 | rustls 메모리 |

- **삭제**: SPAKE2 코드/K 행, 페어와이즈 trust store(peers.json → workspace.json 캐시로 대체 — §7.1).
- v1 키 회전 정책: 장치 identity 회전 = 탈퇴 후 재조인(그룹 키 회전은 D26이 별도 제공). Linux Secret Service 부재 시 **폴백 사다리**(keyutils → 패스프레이즈 argon2 암호화 파일 → 명시 동의 평문+상시 경고)는 v1.1 §4.2 그대로 identity·KEM·MAC 키 공통 적용. 안전 보관 불가 시 workspace 캐시 영속화 비활성.
- 배포 문서에 data_dir 백업/홈 동기화 제외 가이드 유지. 서버 `identity.key` 백업 주의(유출 = 서버 사칭 가능 — 단 pinning 지문이 동일해도 GK가 없어 콘텐츠는 못 봄). 서버 파일 백업 대상·복원 불가 항목은 §7.4에 표로 명시.
- **단일 키 겸용 안전성(D4)**: 로그/회전 서명 입력은 항상 `"sb/log/v1"` 등 도메인 분리 컨텍스트를 **첫 바이트부터** 프리픽스하며, 이 프레이밍은 rustls가 같은 키로 생성하는 TLS `CertificateVerify` 서명 구조(길이·컨텍스트 문자열이 상이)와 **구조적으로 결코 충돌하지 않음**을 §13.1 테스트로 고정한다(교차 프로토콜 서명 재사용 차단). 향후 감사에서 겸용이 부담이면 로그/회전 전용 서명키 분리를 대안으로 기록.

### 4.3 워크스페이스 로그와 초대 프로토콜 (v1.1 §4.3 페어링 전면 대체 — D27·D28)

#### 4.3.1 워크스페이스 로그 (roster 무결성 — 서명 해시체인)

"GK 소지 = 멤버십" 단순 모델은 기각. 이유: **키 회전 시 회전자가 roster의 공개키들로 새 GK를 wrap하므로, 서버가 roster에 공격자 공개키를 1개 주입하면 다음 회전에서 새 GK가 공격자에게 봉인·배달된다.** roster 무결성은 정확히 회전 지점에서 load-bearing이며 멤버 서명 없이는 성립하지 않는다. N≤10·저빈도 멤버십 변경이라 체인 전체 검증 비용은 무시 가능.

```
로그 = 해시체인 엔트리 시퀀스. 엔트리는 저장 바이트열 그대로 보존·전파하고
       서명도 그 바이트열에 수행 — 재직렬화/canonicalization 문제 원천 회피.

Genesis { v, workspace_name, creator_spki(P-256), creator_kem_pk(X25519), created_at }
          — 생성자 자기 서명. workspace_id = BLAKE3(Genesis 바이트열)
Add     { prev_hash, seq, grant_cert, subject_spki, subject_kem_pk, joined_at }
          — grant_sk 서명. grant_cert = { grant_pk, sponsor_device_id, expires_at,
            workspace_id } 를 sponsor 장치키로 서명
Remove  { prev_hash, seq, target_device_id, ts, by } — 잔존 멤버 장치키 서명
Epoch   { prev_hash, seq, epoch_no, rotator_device_id, reason(revoke|manual|periodic),
          member_set_hash, wrapped_bundle_hash, ts } — 회전자 장치키 서명
```

**검증 규칙**(클라이언트, 로드·수신 시 전체 체인): ① 해시 연결·seq 단조 ② 서명자가 해당 seq 시점의 멤버(Remove 이후 서명 무효) ③ grant 만료는 로컬 최초 수신 시각 기준 검사(±24h 허용 오차) ④ **동일 grant_pk의 Add는 체인상 최초 1건만 유효(1회성의 암호학적 근거)** ⑤ epoch_no는 Epoch 엔트리당 정확히 +1 ⑥ **각 멤버는 검증된 체인에서 현 roster(공개키 집합)를 독립 재계산하고 Epoch.`member_set_hash`가 불일치하면 그 Epoch를 거부** — 회전자가 roster 밖 공격자 키를 포함해 새 GK를 wrap하는 것을 탐지(이 검증이 §4.3.1 첫 문단이 명시한 load-bearing 방어의 실체) ⑦ **수신한 wrap 번들에서 계산한 해시가 Epoch.`wrapped_bundle_hash`와 일치**해야 GK 채택(회전자가 커밋한 배포 대상과 실제 배달본의 일치 강제). 서버는 append 순서만 정하는 직렬화 역할 — 어떤 엔트리도 위조 불가.

**포크/오염 엔트리 회복 규칙(신설 — Guest lane Add 미검증 append 대비, §4.5)**: 서버는 서명을 검증하지 않으므로 형식만 온전한 위조/쓰레기 엔트리가 head를 전진시킬 수 있다. 클라이언트에게 **유효 체인 = 검증 규칙 ①~⑦을 통과하는 엔트리들의 최장 접두(longest valid prefix)**이며, 검증 실패 엔트리는 그 지점에서 무시하고 마지막 유효 head 기준으로 상태를 확정한다. 정직 멤버가 `AppendReject(Conflict)`를 받으면 GetLog로 tail을 재취득해 **오염 엔트리를 건너뛰고 자신이 검증한 마지막 유효 head의 prev_hash 위에 재append**한다(오염 엔트리 위에 이어붙이지 않음). 서버는 보완적으로 엔트리의 **서명·prev_hash 형식을 사전 검증**해 명백한 위조 Add를 `AppendReject(Malformed)`로 거부하고(권위는 여전히 클라 검증), Guest lane Add는 유효 locator/blob 소비와 결합될 때만 수락한다(§4.5).

**롤백/split-view 방어(강화 — 필수화)**: 클라이언트는 최후 관찰 log head와 epoch을 workspace 캐시(MAC, D22)와 keychain에 보관하고 후퇴를 거부(단조성). 멤버 간 E2E presence profile에 head 해시 동봉 → **교차 검증은 선택이 아니라 필수**: 관측된 다른 멤버 presence의 head-hash가 자신의 검증된 head와 발산하면 동기화를 차단하고 경고를 띄운다(서버의 선택적 은닉·revoke 은닉을 강제 탐지). presence profile(`enc_profile`) 봉인 내부에는 **단조 seq/epoch/timestamp를 포함**하여 서버가 오래된 profile을 재생(replay)해 head 발산을 은폐하는 것을 거부한다(후퇴 seq 수신 시 폐기). 추가로 **서명된 epoch heartbeat**(신선도 상한 포함) 또는 **다수 멤버 presence 확인 전에는 "최신 epoch" 신뢰를 보류**하여, 완전 격리된 단일 클라이언트의 stale-epoch 지속을 heartbeat 신선도 상한 시간까지로 **시간 제한**한다(무기한 split-view 차단). 그 상한 창 내 잔존은 v1 잔여 위험(§4.1).

#### 4.3.2 워크스페이스 생성 (최초 실행 부트스트랩)

생성자: identity·KEM 키 생성 → 서버 접속(TOFU + 전체 지문 수동 대조 UI, §8.2) → **관리자가 오프라인 생성해 전달한 setup 토큰**과 함께 Genesis 서명·업로드(`ClaimWorkspace`) → 서버는 보유한 `setup_token_hash`와 제시 토큰의 해시 일치만 검증(서버는 평문 토큰을 생성·보관·출력하지 않음 — 침해 서버의 토큰 절취·선점 클레임 차단, §2 조정6) → 최초 Genesis로 워크스페이스 고정, 이후 다른 Genesis 거부(단일 보드) → **GK_0은 생성자 클라이언트가 로컬 생성**(서버 미경유)·keychain 보관 → 관리자는 **창립자 지문(Genesis creator_spki SHA-256)을 멤버에게 공지**(조인 시 out-of-band 대조로 "정당 창립 가장" 차단, §8.2) → 이후 §4.3.3으로 초대. **워크스페이스 복구 불가(brick) 대비 = §7.4 "orphaned/마지막-멤버 복구" 참조.**

#### 4.3.3 초대 발급 (기존 멤버 — 발급 시 서버 연결만 필요, 이후 오프라인 가능)

```
1. code = CSPRNG 60-bit → Crockford Base32 12자, "XXXX-XXXX-XXXX" 표시
2. K_code  = Argon2id(code, salt = workspace_id, m=64MiB, t=3, p=1)   // ~0.5s
   locator = HKDF(K_code, "sb/inv-locator")   // 서버 색인 — 코드 비보유 시 blob 연결 불가
   K_seal  = HKDF(K_code, "sb/inv-seal")
   ※ salt = 공개·불변 workspace_id는 "코드만으로 자체입력(수동 폴백) 시 locator를
     결정적으로 도출"하기 위한 의도적 트레이드오프다. 결과로 한 워크스페이스의 전 초대가
     같은 salt를 공유하며 locator는 code만의 결정적 함수이므로, 방어 근거는 TTL이 아니라
     **60-bit 원시 엔트로피 + Argon2id(64MiB) 메모리 경도**에 있다(§4.3.6). 초대 URI 전달
     경로에 한해 per-invite 랜덤 salt를 URI에 실어 사전계산·상각을 원천 차단하는 옵션을
     둘 수 있으나(수동 코드 폴백은 workspace_id salt 유지), 60-bit에서 전수 사전계산이
     이미 물리적으로 비현실적이라 기본은 단일 salt를 채택.
3. grant 키쌍(P-256, 초대별 일회용) 생성, grant_cert를 자기 장치키로 서명
4. blob = XChaCha20-Poly1305(K_seal) 봉인 {
     grant_sk, grant_cert, workspace_id, 현재 로그 head 해시, 서버 TLS cert 지문 }
   ※ GK는 절대 미포함 (D27)
5. PutInvite{locator, blob, TTL(기본 1h, 최대 24h)}. 코드는 화면 표시 후 zeroize.
   발급자는 초대 UI에서 만료 전 철회(RevokeInvite) 가능. 발급 기기당 활성 초대 1개.
```

#### 4.3.4 조인 (새 기기 — 입력: 서버 주소 + 코드, 초대 URI면 붙여넣기 1회)

```
1. 새 기기: 장치 identity(P-256, TLS·로그 서명 겸용) + X25519 KEM 키 생성, keychain 보관.
2. 서버 TLS 접속(최초 TOFU, Guest lane) → 코드로 K_code·locator 계산 → GetInviteBlob
   → K_seal 복호. 실패 = 오타/만료/위조 서버 → "코드가 올바르지 않거나 만료됨".
   ** AEAD 성공 자체가 상호 인증: 코드 없는 가짜 서버는 유효 blob 제조 불가 → 공격자
      워크스페이스 유인 차단. blob 내 서버 cert 지문으로 TOFU 소급 검증·pinning 확정,
      workspace_id·head 해시로 이후 받는 로그를 검증. **
3. GetLog → 체인 전체 검증(§4.3.1, blob의 head 해시 이상까지 도달해야 유효).
4. Add 엔트리 작성{자기 SPKI, KEM pk, grant_cert} → grant_sk 서명 → AppendEntry.
   grant_sk·code 즉시 zeroize. 서버는 blob 삭제·consumed 처리(편의적 조기 차단).
5. GK 전달: 온라인 멤버 클라이언트가 LogAppended(Add) 수신 → 체인 검증(grant 서명 →
   sponsor 서명 → 멤버 자격의 위임 사슬) → 현재 GK_e를 새 기기 KEM pk로 wrap
   (X25519 ECDH-ES + HKDF + XChaCha20-Poly1305, AAD = {workspace_id, epoch,
   log head 해시, target, wrapper}) → PutKeyUpdate + UI 알림 "기기 <이름>이
   <발급자>의 초대로 참여". 복수 멤버의 중복 wrap은 무해(멱등).
6. 새 기기: Member lane 재접속 → Welcome.pending_key_update 또는 KeyUpdatePush로
   unwrap → GK_e 확보 → head 수렴 → 동기화 개시. 전원 오프라인이면 "참여 승인됨 —
   멤버 온라인 시 자동 완료" 대기(발급자가 온라인인 통상 케이스에서는 즉시 완료).
```

#### 4.3.5 1회성과 서버 신뢰 — 불필요

서버의 consumed 플래그는 편의일 뿐. 보안 근거는 (i) 체인 규칙 "grant당 Add 최초 1건"(중복 조인은 전 멤버 검증에서 무효), (ii) blob에 GK 부재(악의 서버가 blob을 영구 보존·재제공해도 코드 없이는 무용, TTL 후엔 코드가 있어도 무용). 잔여: 코드 유출 + 서버가 공격자 Add를 먼저 배치하는 결합 시나리오 — 조인 알림 + revoke·회전으로 대응(§4.1).

#### 4.3.6 코드 엔트로피 분석 (수치 요약)

전제: Argon2id(m=64MiB, t=3, p=1) ≈ 0.5s/평가. 공격자 rig 64GiB RAM ≈ 1,024 병렬 ≈ 2^11 추측/s(공격자에게 후한 추정). 방어자 비용: 조인·발급 시 1회 ~0.5s + 64MiB 일시 점유(RSS 목표와 양립하는 일시 스파이크).

- **blob에 GK가 있었다면(기각안)**: 영구 대입 표면 — Base32 8자(40-bit)는 100 rigs에서 평균 31일로 탈락, 16자(80-bit)가 필요했음.
- **채택안(D27, GK 미포함) — 정정된 근거**: salt=workspace_id가 공개·불변이라 공격자는 TTL과 무관하게 code→locator/K_seal **사전계산 테이블**을 만들 수 있고, locator가 코드만의 결정적 함수라 사전계산+온라인 locator 프로빙으로 코드를 역추적할 수 있다. 따라서 **"TTL이 대입 총량을 제한한다"는 논거는 폐기**하고, 방어 근거를 **60-bit 원시 엔트로피 + Argon2id(64MiB) 메모리 경도의 사전계산 비용**으로 정정한다: 60-bit 전 공간의 Argon2id-64MiB 사전계산은 ≈2^60 회 평가·각 64MiB로 **연산·저장 모두 물리적으로 비현실적**(공격자 100배 가속·1,000 rigs 극단 가정에서도 전수 테이블 구축 불가). 반면 40/50-bit는 사전계산이 TTL에 제한되지 않으므로(예: 40-bit는 전량 사전계산 후 영구·즉시 크랙) TTL 기준으로 기각했던 이전 분석은 과소보수적 — **40/50-bit는 사전계산 관점에서 기각**, 60-bit만 안전.
- **확정: Crockford Base32(I/L/O/U 제외) 12자, 4-4-4 그루핑, 60-bit.** 근거는 위와 같이 원시 엔트로피이며 TTL은 코드 유출 시의 노출 창 축소(운영 편의)로만 활용한다. 한 워크스페이스 전 초대가 salt를 공유하는 **상각 이점**은 공격자에게 넘어가나 60-bit에서 무의미(§4.3.3 주석). 체크섬 문자 미채택(오타는 locator 불일치/AEAD 실패로 검출, 시도당 0.5s Argon2가 온라인 시도를 자연 스로틀 — Guest 세션·IP당 GetInviteBlob 횟수 상한과 병행, §5.6).

### 4.4 멤버 제거와 키 회전 (epoch — D26)

**회전 절차**

```
1. revoke 실행 멤버: Remove 엔트리 append → 즉시 GK_{e+1} = 256-bit CSPRNG
   (구 키에서 비파생 — 구 키 유출이 신 키를 손상시키지 않음, 회전 경계의 PCS)
2. Epoch 엔트리 append(reason=revoke) → **채택된 그 Epoch 로그 엔트리 해시**를 확보 →
   잔존 멤버 전원의 KEM pk로 각각 wrap(§4.3.4-5 구조, AAD = {epoch e+1·head·**Epoch 엔트리 해시**·member_set_hash})
   → PutKeyUpdate(서버 mailbox). 멤버는 수신 wrap의 `from`·Epoch 엔트리 해시·member_set_hash가
   자신이 검증한 체인의 Epoch(e+1)와 **일치할 때만 GK를 채택** — 동시 회전 경합에서 고아(패자)
   Epoch의 wrap을 서버가 배달해도 멤버 간 GK 분열이 발생하지 않음(Minor 회전 경합 방어).
3. 서버(정직 시): 제거 기기 연결 즉시 드롭·admission 삭제 — 심층 방어일 뿐 신뢰 근거 아님.
   실질 차단은 "GK_{e+1}을 모름"이 담당.
4. 회전자는 자기 현재 head를 새 epoch로 재봉인·재발행 — 기존 멤버에겐 LWW no-op,
   신규 접속자의 head 수렴용(서버 head 캐시는 epoch 전환 시 무효화).
```

**트리거**: revoke 시 **필수·자동**(원자 플로우, 생략 불가) / 초대 이상 징후(만료 grant 조인 시도 등) 시 UI 권고 / 수동 "키 회전" 버튼 / 주기 회전(기본 off, 30/90일 옵션).

**오프라인 멤버의 늦은 수신**: mailbox는 멤버별 **최신 KeyUpdate만** 보관 — 중간 epoch 스킵 직행 가능(항목은 최신 1건 수렴 원칙이라 중간 epoch 키 불필요). 재접속 시 로그 tail 검증 → Epoch 확인 → unwrap → 구 GK zeroize → head 재수렴. 자기 wrap 누락(회전자 크래시 등) 시 E2E "re-wrap 요청" 게시 → **이미 GK_{e+1}을 보유한 임의 온라인 멤버**가 재wrap(동일 메커니즘 재사용).

**이행 불가 Epoch 회복(신설 — Minor 17)**: 회전자가 Epoch e+1을 append한 뒤 **어떤 wrap도 업로드하기 전에 크래시/디스크 손실**되면, 죽은 회전자 외 누구도 GK_{e+1}을 갖지 못한 채 체인에 이행 불가능한 e+1이 커밋된다(re-wrap 요청에 응답할 주체 없음). 규칙: **타임아웃 내 자신 몫 wrap을 얻을 수 없는 Epoch는 폐기 대상으로 간주**하고, 임의 온라인 멤버가 fresh 랜덤 키로 **e+2를 상위 커밋**해 대체한다(동시 회전 경합 패자와 동일 폐기 메커니즘 재사용). 이 회복 경로는 §13.1/§13.2에 테스트로 명시.

**epoch 전환 규칙**: 송신은 새 epoch 인지 즉시 신 키만. 수신 grace는 reason=manual/periodic이면 구 epoch 항목 10분 수용(전파 지연 중 유실 방지) 후 구 키 zeroize, **reason=revoke이면 grace 0**(제거 멤버의 구 키 LWW 주입 창 차단). 동시 회전 경합: Epoch 엔트리도 체인이므로 서버 직렬화상 최초의 e+1만 유효, 패자는 tail 재취득 후 e+2로 재발행. 멤버는 위 step 2대로 wrap을 **채택된 Epoch 엔트리 해시에 바인딩**해 채택하므로, 서버가 패자(고아) Epoch의 wrap을 일부 멤버에게 배달해도 채택되지 않아 GK 분열이 없다.

**생성자(Genesis 서명자) 기기의 정상 제거**: 생성자는 Genesis 이후 특별 권한이 없다 — Remove·회전은 잔존 멤버 누구나 서명하므로 생성자도 **다른 멤버가 Remove+Epoch로 일반 멤버와 동일하게 제거 가능**(단 생성자가 유일 멤버일 때는 orphan 복구 규칙 적용 — §7.4). "생성자는 못 지운다/지우면 워크스페이스가 죽는다"는 오해 없음.

### 4.5 전송 암호화와 네트워크 격리

**TLS (클라↔서버, 단일 포트 45871)**
- rustls 0.23 + tokio-rustls 0.26, **TLS 1.3 전용**. 서버 = 자기서명 cert, 클라이언트는 SHA-256(SPKI) 지문 pinning(초대 blob 경유 검증으로 TOFU가 암호학적으로 승격 — §4.3.4).
- 클라 = mTLS 장치 cert. 서버 custom `ClientCertVerifier`: 지문이 로그 admission 목록에 존재 → **Member lane**, 미지 cert → **Guest lane** 태깅(거부 아님). 체인/유효기간/호스트명 검증은 생략, 지문 단일 비교(v1.1 원칙 계승).
- **Guest lane** 허용 메시지 = `ClaimWorkspace`(미클레임 시), `GetInviteBlob`, `GetLog`, `AppendEntry(Add)`뿐. 상한: IP당 동시 1, 전체 8, 프레임 32KiB, 세션 TTL 60s, IP당 신규 3/분, **세션·IP당 `GetInviteBlob` 소수(기본 5회) 상한**(온라인 열거 스로틀 — Minor) — v1.1 "페어링 창 동안만 미지 인증서 수락" 원칙의 계승.
- **Guest `GetLog` 게이트(Minor)**: 서버는 해당 Guest 세션이 **직전에 유효 blob을 `GetInviteBlob`으로 반환받은(=대응 `PutInvite`가 존재하는 locator) 이후**에만 `GetLog`를 허용한다(코드 소지 증명). 코드 없는 임의 LAN 호스트의 roster·workspace_name 열람을 차단(§4.1 메타데이터 행).
- **Guest `AppendEntry(Add)` admission 바인딩(핵심 — Major)**: 서버가 서명을 검증하지 못하는 blind 모델에서 임의 Guest의 쓰레기 Add 무제한 append(로그 영구 팽창·자기 admission·슬롯 점유)를 막기 위해, 서버는 **서명 검증 없이도 가능한 상태 기반 게이트**를 강제한다 — `AppendEntry(Add)`는 (a) 그 Guest 세션이 방금 유효 blob을 `GetInviteBlob`한 **미소비 locator**에 대응하고, (b) 그 locator의 `PutInvite`가 미소비 상태이며, (c) Add의 `grant_cert`가 그 초대 blob과 결합될 때만 수락한다. **초대당 Add 1건 상한**(수락 즉시 locator consumed), 그 외 Add는 `AppendReject(NotAuthorized)`. 이는 서명 검증이 아니라 서버가 이미 가진 초대 상태만 사용하므로 blind-relay 모델과 양립한다. 병행하여 서버는 엔트리의 **서명·prev_hash 형식을 사전 검증**해 명백한 위조를 `AppendReject(Malformed)`로 거부(권위는 여전히 클라 체인 검증, §4.3.1).
- **로그 총량 상한·체크포인트**: 로그 총 엔트리 수·바이트에 상한(초과 시 신규 Add 거부·운영 경보). 조인 비용 무한 팽창 방지를 위해 **로그 체크포인트/컴팩션**을 규정 — 다수 멤버가 서명한 최신 roster 스냅샷을 checkpoint 엔트리로 커밋하고, 신규 조인은 최신 checkpoint 이후만 전량 검증(이전은 checkpoint 해시로 요약 신뢰). 상세 파라미터는 §7.4.
- TLS 위 애플리케이션 계층에서 `Hello.device_id ≠ TLS cert 지문`이면 즉시 종료(계층 간 신원 일치 강제 — v1.1 유지). 핸드셰이크 타임아웃 3s, 실패 IP token bucket.
- 동일 device_id 중복 연결은 신규가 기존을 대체(재연결 레이스 해소). v1.1의 동시 다이얼 tie-break는 소멸.

**네트워크 격리**
1. **주소 allowlist(v1.1 §4.5 승계, sb-proto로 이관)** — 서버 bind·accept, 클라이언트 아웃바운드 다이얼 공통: IPv4 `10/8`·`172.16/12`·`192.168/16`·`169.254/16`·loopback / IPv6 `fc00::/7`·`fe80::/10`(zone id 검증)·`::1`. 그 외 전부 거부(전역 IPv6 포함), 바이트를 읽기 전 close.
2. 서버: `0.0.0.0`/`::` 바인딩 금지 — server.toml `bind_addr`을 allowlist로 검증, 위반 시 기동 거부. systemd `IPAddressAllow/Deny`가 커널 수준에서 이중 강제(§7.4).
3. 클라이언트: 아웃바운드 목적지는 **설정된 서버 주소 1개뿐**(allowlist 검증 후) — 코드 레벨 거부. 인바운드 리스너 없음(D19). 텔레메트리·업데이트 체크 0건(통합 테스트 소켓 후킹 검증).
4. 자기 주소 변경(D24): 리스너 재바인딩·mDNS 재등록 소멸 — 감지 시 백오프 리셋 + 즉시 재다이얼만.

### 4.6 데이터 보호

**와이어 콘텐츠 E2E 봉인 (신설 — D26·D31)**
- 파생: `k_sig = HKDF(GK_e,"sb/sig-v1")`(SignalBody), `k_body = HKDF(GK_e,"sb/body-v1")`(콘텐츠 본문), `k_cid = HKDF(GK_e,"sb/cid-v1")`(ContentId) — 도메인 분리로 서버의 블롭 교차 스플라이싱 차단.
- 봉인: XChaCha20-Poly1305, 24B 랜덤 nonce를 암호문 앞에 전치. **AAD = 평문 헤더(SignalHdr) 캐노니컬 CBOR ‖ origin device_id** — 서버가 헤더를 변조/치환하면 수신 클라의 AEAD open 실패.
- 콘텐츠 본문: `(zstd?)plaintext`를 k_body로 통짜 1회 봉인 → 암호문을 CHUNK_SIZE로 분할 전송. 압축은 **암호화 전**(4KiB 초과 텍스트 — 암호문은 비압축성). 수신 검증 = AEAD tag + 복호 후 k_cid 재계산 일치.
- epoch 회전 시 k_cid도 바뀌어 epoch 간 dedup은 끊김(회전은 드물어 수용). 평문 `ct_size` 노출은 v1.1의 크기 메타데이터 수용과 동일 선상.

**로컬 저장 (v1.1 승계)**
- 히스토리: SQLite content 필드만 XChaCha20-Poly1305(row별 24B nonce, AAD = id‖kind‖created_at), dedup은 keyed BLAKE3(D21), WAL 암호문만, `secure_delete=ON`, 전체 삭제 = crypto-erase(키 파기·재생성).
- concealed 감지/재표기·검사 원자성(동일 클립보드 세션)·기본 excluded_apps 동봉: v1.1 §4.6 그대로.
- 메모리: zeroize/secrecy로 키·코드·전송 버퍼 wipe. 평문을 IPC로 넘기지 않는 구조가 1차 방어.
- Tauri 2 capability 최소·CSP `default-src 'self'; img-src 'self' data: blob:; connect-src ipc: http://ipc.localhost`·remote capability 금지·release devtools 비활성: v1.1 그대로.

### 4.7 로깅과 민감정보 (D23 — 클라이언트·서버 공통)

- tracing + tracing-subscriber + tracing-appender. 클라: `data_dir/logs/`(0700/0600), 일 단위 회전, 보존 7일, 기본 INFO. 서버: stdout 기본(journald/Docker), `log_dir` 설정 시 동일 회전.
- **금지 규칙**: 클립보드 콘텐츠 평문·미리보기, **초대 코드/URI**, 키 재료(GK·K_code·grant_sk·wrap 평문·세션 키·keychain 키), 초대 blob·KeyUpdate payload는 어떤 레벨에서도 로깅 금지. 콘텐츠는 keyed 해시 앞 8바이트 + 크기 + kind로만, 장치는 fp 앞 8바이트로만 지칭.
- panic hook 로컬 기록(외부 전송 없음). 진단 번들(§8.6)은 로그 + 서버 주소·지문 + 익명화 상태만 — 초대 코드·콘텐츠 불포함. §13.2에 "로그 파일 내 평문·초대 코드 미포함" 검증(서버 로그 포함).

---

## 5. 동기화 프로토콜 (v1.1 §5 대체 — PROTO = 2)

### 5.1 계층 모델과 프레이밍

```
[전송 계층]  클라 ↔ 서버 TCP + rustls TLS 1.3 (서버 지문 pinning + 클라 mTLS — §4.5)
[E2E 계층]   그룹 키 GK_e (클라이언트들만 보유, epoch 회전 — §4.6)
  콘텐츠·kind·정렬키·inline·장치명(profile)은 전부 GK AEAD 암호문 — 서버는 opaque bytes로만
  취급 = blind relay. 서버 침해 시 노출은 §4.1의 평문 메타데이터로 한정.
```

- 프레이밍: `tokio_util::codec::LengthDelimitedCodec`(u32 LE, `max_frame_length = 256KiB` — Guest lane은 32KiB). 초과 프레임 = 프로토콜 위반, 즉시 종료.
- 직렬화: serde + ciborium(CBOR). 마이너 확장 = `#[serde(default)]` 신규 필드, variant 추가 = 버전 증가, 지원 창 = 직전 1개(협상은 Hello/Welcome, 게이트는 서버가 수행). 전부 v1.1 §5.1 승계.

### 5.2 메시지 정의 (Rust enum 스케치)

```rust
pub type DeviceId  = [u8; 32];   // SHA-256(cert SPKI) — TLS 계층 신원과 동일 (D4)
pub type ContentId = [u8; 32];   // keyed BLAKE3(k_cid[epoch], plaintext) — D21(개정)
pub type Epoch     = u64;        // 그룹 키 세대(단조 증가). 서버는 번호만 안다
pub type Locator   = [u8; 32];   // HKDF(K_code, "sb/inv-locator") — 초대 blob 색인

pub const PROTO_MIN: u16 = 2;
pub const PROTO_MAX: u16 = 2;

#[derive(Serialize, Deserialize)]
pub struct Envelope<M> { pub v: u16, pub msg: M }   // Envelope<C2s> / Envelope<S2c>

// ───────────────────────── 클라 → 서버 ─────────────────────────
#[derive(Serialize, Deserialize)]
pub enum C2s {
    // 세션 수립 (Member lane, mTLS 확립 직후)
    Hello(Hello),

    // Guest lane (미등록 cert — 아래 4종 외 수신 시 즉시 종료)
    ClaimWorkspace { token: String, genesis: Vec<u8> },  // 부트스트랩(§4.3.2): setup 토큰 + Genesis
    GetInviteBlob  { locator: Locator },
    GetLog         { from_seq: u64 },                    // Member lane에서도 사용(tail 동기화)
    AppendEntry    { entry: Vec<u8> },                   // 저장 바이트열 그대로. Guest는 Add만,
                                                        //  Member는 Remove/Epoch도. 서버는 직렬화만(D28)
    // 초대 관리 (Member)
    PutInvite    { locator: Locator, blob: Vec<u8>, ttl_s: u32 },  // blob ≤ 4KiB, TTL ≤ 24h
    RevokeInvite { locator: Locator },

    // 키 배포 (Member)
    PutKeyUpdate { updates: Vec<KeyUpdate> },            // 회전/조인 wrap 업로드 → mailbox

    // 동기화 (Member)
    ClipSignal     { hdr: SignalHdr, e2e: Vec<u8> },     // e2e = seal(k_sig, SignalBody, aad=hdr‖origin)
    ContentRequest { id: ContentId, epoch: Epoch },      // 서버가 소스(캐시/원본) 결정
    ContentBegin   { id: ContentId, ct_size: u64, chunk_count: u32, chunk_size: u32 }, // ContentPull 응답
    ContentChunk   { id: ContentId, index: u32, data: Vec<u8> },   // 암호문 조각
    ContentAbort   { id: ContentId, reason: AbortReason },   // Superseded | Cancelled | InternalError
    ContentReject  { id: ContentId, reason: RejectReason },  // origin의 Pull 거절: Gone | TooLarge | Busy

    // 기타
    SetProfile { epoch: Epoch, e2e: Vec<u8> },           // GK 봉인 {name, platform, log_head_hash}
    Leave,                                               // 자발 탈퇴(잔존 멤버 UI가 회전 권고)
    Ping { nonce: u64 },
    Bye  { reason: ByeReason },                          // Shutdown | ProtocolError
}

// ───────────────────────── 서버 → 클라 ─────────────────────────
#[derive(Serialize, Deserialize)]
pub enum S2c {
    Welcome(Welcome),                                    // Hello 응답 — v1.1 HelloAck 대체

    // Guest 응답
    InviteBlob   { blob: Option<Vec<u8>> },              // None = 미존재/만료(구분 없음 — 정보 최소화)
    LogEntries   { entries: Vec<Vec<u8>>, done: bool },  // 저장 바이트열 그대로
    AppendAck    { seq: u64, head_hash: [u8; 32] },
    AppendReject { reason: AppendRejectReason },         // NotAuthorized | Conflict(tail 재취득) | Malformed

    // 동기화
    SignalFanout { origin: DeviceId, hdr: SignalHdr, e2e: Vec<u8> },  // origin은 서버가 인증 세션에서
                                                        //  스탬프 — 발신자에게는 미반송
    ContentPull  { id: ContentId },                      // → origin: 업로드 요청(요청자 다수는 서버가 티잉)
    ContentBegin { id: ContentId, ct_size: u64, chunk_count: u32, chunk_size: u32 },
    ContentChunk { id: ContentId, index: u32, data: Vec<u8> },
    ContentReject{ id: ContentId, reason: RejectReason },  // + OriginOffline | Gone
    ContentAbort { id: ContentId, reason: AbortReason },

    // 멤버십/키/presence
    LogAppended  { entry: Vec<u8>, seq: u64 },           // 신규 엔트리 실시간 전파(체인 검증은 클라)
    KeyUpdatePush{ wrap: Vec<u8> },                      // 본인 몫 wrap 즉시 push
    Presence     { device_id: DeviceId, online: bool, enc_profile: Option<Vec<u8>> },
    Revoked,                                             // 본인 제거 "힌트"(인증 안 됨) → 로그 tail 재동기화·
                                                        //  서명 검증만 트리거. 키 파기·crypto-erase 금지
                                                        //  (검증된 Remove(self)+사용자 확인 이중 게이트, §5.4)

    // 기타
    Pong  { nonce: u64 },
    Error { code: ErrorCode, detail: String },           // RateLimited | NotMember | VersionIncompatible |
                                                        //  Busy | InviteFull | ...
    Bye   { reason: ByeReason },                         // Shutdown | Revoked | ProtocolError
}

#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub device_id: DeviceId,        // TLS cert 지문과 일치 강제 (§4.5)
    pub proto_min: u16, pub proto_max: u16,
    pub app_version: String,
    pub epoch: Epoch,               // 보유 최신 epoch — 서버가 밀린 wrap 배달 판단
    pub log_head: (u64, [u8; 32]),  // 보유 로그 (seq, hash) — 서버가 tail 전송 판단
}

#[derive(Serialize, Deserialize)]
pub struct Welcome {
    pub chosen_version: u16,
    pub epoch: Epoch,                        // 서버가 아는 현재 epoch 번호(로그 기준)
    pub log_tail: Vec<Vec<u8>>,              // Hello.log_head 이후 엔트리(클라가 체인 검증)
    pub pending_key_update: Option<Vec<u8>>, // 오프라인 중 회전/조인 wrap(본인 몫 최신 1개)
    pub presence: Vec<(DeviceId, bool, Option<Vec<u8>>)>,   // 전 멤버 + 온라인 + enc_profile
    pub head: Vec<(DeviceId, SignalHdr, Vec<u8>)>,  // 최근 HEAD_CACHE_DEPTH(4)건 signal —
                                             //  클라가 복호 후 LWW로 최신 1건 판정(D31 귀결)
    pub server_time_ms: u64,                 // 진단 전용(시계 오차 로그) — 판정 사용 금지
}

/// 평문 헤더 — 서버가 보는 전부 (D31)
#[derive(Serialize, Deserialize, Clone)]
pub struct SignalHdr {
    pub id: ContentId,     // keyed — 서버 역산 불가
    pub epoch: Epoch,      // stale-key 감지 + 서버 캐시 무효화 기준
    pub ct_size: u64,      // fetch 계획/캐시 상한 판단
}
// 주의: origin·kind·lamport·wall_ts·inline은 평문 헤더에 없음 — 전부 E2E 내부

#[derive(Serialize, Deserialize)]
pub struct KeyUpdate { pub to: DeviceId, pub epoch: Epoch, pub wrap: Vec<u8> }

// ───────────── E2E payload (서버 파싱 불가 평면) ─────────────
#[derive(Serialize, Deserialize)]
pub struct SignalBody {                 // ClipSignal.e2e = seal(k_sig, ·, aad=SignalHdr‖origin)
    pub kind: ContentKind,              // Text | ImagePng
    pub plain_size: u64,                // 원문 크기(압축·암호화 전)
    pub lamport: u64,                   // LWW 1차 키 — 서버 비가시
    pub wall_ts_ms: u64,                // LWW 2차 키 + UI 표시
    pub origin: DeviceId,               // 서버 스탬프 origin과 대조(AAD로도 결합)
    pub compressed: bool,               // zstd 적용 여부
    pub inline: Option<Vec<u8>>,        // ≤32KiB 텍스트(압축 후) — 존재 여부도 은닉됨
}

#[derive(Serialize, Deserialize)]
pub struct RotationBlob {   // KeyUpdate.wrap 내부 — X25519 ECDH-ES + HKDF + XChaCha20-Poly1305 봉인,
                            //  AAD = {workspace_id, epoch, log head 해시, epoch_entry_hash, member_set_hash, to, from}
    pub new_epoch: Epoch,
    pub group_key: [u8; 32],
    pub reason: EpochReason,            // Revoke(DeviceId) | Manual | Periodic | Join(현 epoch 전달)
    pub epoch_entry_hash: [u8; 32],     // 회전: 체인에 채택된 Epoch(e+1) 엔트리 해시 / Join: 현 log head 해시.
                                        //  구 prev_epoch_confirm=H(GK[e-1]) 대체 — 비밀키 해시는 현·구 멤버
                                        //  전원이 계산 가능해 서명 이상을 증명 못 하고 Join에선 검증 불가한
                                        //  죽은 필드였음. wrap을 서명·체인 검증되는 엔트리 해시에 결속(Minor)
    pub member_set_hash: [u8; 32],      // 수신자가 검증한 Epoch.member_set_hash와 일치할 때만 GK 채택(§4.4 step 2)
    pub from: DeviceId,
    pub sig: Vec<u8>,                   // 작성자 identity 서명(domain-sep) — 로그 멤버 자격 검증
}

// ───────────── 워크스페이스 로그 엔트리 (§4.3.1 — 바이트열 보존·서명) ─────────────
#[derive(Serialize, Deserialize)]
pub enum LogEntry {
    Genesis { v: u16, workspace_name: String, creator_spki: Vec<u8>, creator_kem_pk: [u8; 32],
              created_at: u64, sig: Vec<u8> },
    Add     { prev_hash: [u8; 32], seq: u64, grant_cert: GrantCert, subject_spki: Vec<u8>,
              subject_kem_pk: [u8; 32], joined_at: u64, sig: Vec<u8> },       // grant_sk 서명
    Remove  { prev_hash: [u8; 32], seq: u64, target: DeviceId, ts: u64,
              by: DeviceId, sig: Vec<u8> },                                    // 잔존 멤버 서명
    Epoch   { prev_hash: [u8; 32], seq: u64, epoch_no: Epoch, rotator: DeviceId,
              reason: EpochReason, member_set_hash: [u8; 32],
              wrapped_bundle_hash: [u8; 32], ts: u64, sig: Vec<u8> },          // 회전자 서명
    RotateKem { prev_hash: [u8; 32], seq: u64, subject: DeviceId,              // 본인 서명 —
                new_kem_pk: [u8; 32], ts: u64, sig: Vec<u8> },                 //  KEM 수신키 회전(§4.1 축 B)
}
#[derive(Serialize, Deserialize)]
pub struct GrantCert {                  // sponsor 장치키 서명
    pub grant_pk: Vec<u8>, pub sponsor: DeviceId, pub expires_at: u64,
    pub workspace_id: [u8; 32], pub sig: Vec<u8>,
}
```

### 5.3 순서 판정(LWW)과 에코 방지

```
key(item) = (lamport, wall_ts_ms, origin_device_id)   // 사전식 비교 — v1.1 D10 판정식 그대로
key(incoming) > key(current_applied) 일 때만 로컬 클립보드 반영
```
- lamport: 로컬 복사 +1, 원격 수신 max-merge. 동시 복사는 wall-clock, 완전 동률은 device_id tie-break → 전 노드 수렴. 패자 콘텐츠는 히스토리 보존. **판정 주체는 클라이언트뿐**(정렬키가 E2E 내부 — D10 개정): 서버는 순서를 보장하지도 조작하지도 못하고, old-signal replay는 LWW 패배·CID 중복으로 자연 무해화, epoch 경계 밖 재생은 복호 실패.
- 정렬키는 송신 멤버가 채우므로 **침해된 멤버**의 조작(항상 승자화)은 여전히 가능 — 고위험 패턴 확인(§3.3)으로 완화, §4.1 위협표 존치.

**에코 방지 — 3중 → 2중으로 단순화**
1. **구조적 no-echo**: 서버가 fanout에서 발신자 제외 + 클라이언트는 수신 콘텐츠 절대 재발행 금지 + 경로가 단일 허브 — 다단 루프·중복 경로 자체가 불가능(v1.1 "릴레이 금지" 필러가 토폴로지 보장으로 대체 — D9 폐기).
2. **suppress set 유지**: 원격 콘텐츠를 OS 클립보드에 쓰면 로컬 워처가 발화하는 문제는 토폴로지 무관 — v1.1 §5.3-1 그대로(등록 후 2s 유예 창, PNG 정규화 해시 포함).
- v1.1 recent-hash LRU(16)는 다중 경로 중복 방어였으므로 **필수→선택 강등**(방어적 유지 무방).

### 5.4 FSM

**클라이언트 연결 FSM (서버 1대 — v1.1 피어별 FSM 대체)**

```
[Idle] → 기동/설정 로드 → [Connecting: TCP+TLS, 서버 지문 pinning, 3s 타임아웃]
   실패 → [Backoff: 1s→×2→상한 30s, jitter ±20%, wake/네트워크 변경(D24) 시 리셋]
 → [Authenticating: Hello→Welcome, 10s 타임아웃]
     Welcome.epoch > 보유 epoch → log_tail 검증 → pending_key_update unwrap
       → 실패 시 [KeyStale: UI 경고 "동기화 불가 — 키 갱신 대기", re-wrap 요청 게시]
 → [Ready]: Welcome.head 복호→LWW 판정→최신 1건 적용, presence/로그 반영.
     Welcome.head가 비어 있으면(서버 재시작) 자기 최신 항목 ClipSignal 재발행 — 수 초 내 재수렴
     ←→ [Degraded: 15s 무수신 시 Ping, 45s 무수신 → 사망 판정] → [Backoff]
[Join 분기 — Guest lane] Connecting(미등록 cert, TOFU)
  → GetInviteBlob → K_seal 복호(실패 = 오타/만료 → 종료) → 지문 소급 검증·pinning 확정
  → GetLog → 체인 전체 검증 → AppendEntry(Add) → AppendAck → grant_sk·code zeroize
  → 재접속(Member lane) → Hello/Welcome → pending_key_update 또는 KeyUpdatePush로 GK 확보
  → 없으면 [AwaitKey: "멤버 온라인 시 자동 완료"] → 수신 시 Ready
Revoked/Bye{Revoked} 수신 → **힌트로만 처리**: 로그 tail 재동기화·서명 검증만 트리거(키 미파기).
  → 검증된 체인에 서명된 Remove(self) 엔트리가 **실제 존재할 때만** 이탈 확정: 로컬 GK 파기 →
    히스토리 crypto-erase는 [검증된 Remove(self) + 명시적 사용자 확인] **이중 게이트** → [Idle(미가입)].
  → Remove(self) 부재면 위조 통지(반신뢰/침해 서버)로 간주해 **무시**(강제 이탈·복호 불가·데이터 손실 차단).
```

**서버 세션 FSM**: `Accept → TLS → {admission 지문 → AuthWait(Hello, 10s) → Active} | {미지 cert → Guest(허용 4종만, TTL 60s)}`. Active: fanout/relay/presence, 90s 무수신 세션 정리, 동일 device_id 중복 연결은 신규가 대체.

**수신측 콘텐츠 전송 FSM — v1.1 유지 + 검증만 교체**: Evaluate(복호한 SignalBody의 key/크기/종류) → Requesting → **ContentBegin 검증**(ContentBegin는 E2E 인증 안 된 서버/origin 평문 메시지 — 반드시 **인증된 signal의 `SignalHdr.ct_size`와 대조**, 불일치 시 즉시 `Abort`; `chunk_count ≤ ceil(ct_size/CHUNK_SIZE)` 및 `MAX_CONTENT_SIZE` 상한을 **수신·할당 전에** 강제 — 악의 서버의 과대 ct_size/chunk_count 메모리/대역 DoS 차단) → Receiving(index 순서 검증, 더 높은 key signal 도착 시 `Abort(Superseded)`) → Verifying(**AEAD open + keyed CID 재계산**, 불일치 1회 재요청) → Applying(suppress 등록 → set → lamport merge). 클라 동시 fetch 1.

**fetch 라우팅 — 서버가 결정, 요청자에게 투명**

```
메커니즘 = cache-through 후 fan-out (라이브 티잉 폐기 — Major)
요청자→S: ContentRequest{id, epoch}
  S: ① 본문 캐시에 CID 있으면 → 완결된 Arc<Bytes>에서 요청자별 **독립 커서**로 서빙
        (복사 없음. 느린/늦은 요청자는 자기 커서로 진행 — 상호 head-of-line 블로킹 없음)
     ② 없고 CID 보유자(origin 또는 **해당 CID를 이미 받은 임의 온라인 멤버**)가 있으면 →
        보유자에 ContentPull{id} → **1회 업로드를 크기 상한(≤max_content_bytes) 본문 캐시로
        버퍼링**(backpressure는 보유자→서버 구간에만 적용) → 완결 후 ①로 서빙
     ③ 보유자 없음(전원 오프라인/송신 캐시 만료) → ContentReject{Gone|OriginOffline}
청크는 GK 암호문 조각 — 서버는 저장·중계 모두 blind. 본문이 암호문이므로 origin이 아니어도
보유 멤버가 응답 가능(②) → 승자 가용성 대폭 개선.
```
- **head 본문 캐시(D31 정합 재정의 — Major)**: 서버는 순서를 판정하지 못하므로(D31) "최신 1건"은 정의 불가다. 대신 **head signal(HEAD_CACHE_DEPTH개) 각각의 CID 본문을 캐시**(총량 상한 = `CONTENT_CACHE_BYTES`, 예: ≤4×max_content_bytes 또는 더 작은 캡; 초과 시 LRU 축출) → 클라가 LWW로 고른 승자 signal X의 본문을 서버가 보유할 확률을 높인다. 그래도 승자 본문 미보유 가능성은 남으므로 **head 본문 가용성은 best-effort**로 명시하고, 이때 ②의 보유 멤버 릴레이로 회복하며, 통합 테스트의 head 재수렴 기준은 "보유자 존재 시 승자 fetch 성공, 전원 미보유 시 재발행으로 수렴"으로 정의(비결정적 실패 방지).
- **서버 메모리 상한**: `동시 서로 다른 본문 수 × max_content_bytes ≤ CONTENT_CACHE_BYTES`로 정의하고 `MemoryMax=128M`(§7.4) 내에 드는지 확인(동시 fetch·fan-out 정책과 함께 실측 — §13.3). epoch 전환 시 head 본문 캐시 전체 무효화(§4.4-4의 재발행이 재충전).

### 5.5 엣지케이스 처리 방침 (요지)

- 동기화 off: 발행 중단 + 수신 무시 — **로컬 정책으로 전환, 와이어 통지 없음**(v1.1 `SyncState` 폐지. 서버·타 멤버가 알 필요 없는 정보를 와이어에서 제거). 재개 시 head 재수렴.
- 빈 클립보드/클리어 미전파, 연타 debounce, 다중 포맷 우선순위(concealed 전체 제외 > 텍스트 > 이미지, 단일 kind 전파), 텍스트 정규화(CRLF/LF 변환 금지, X11 UTF8_STRING 우선), set 실패 폴백, X11 lazy/INCR, Windows OpenClipboard 경합 백오프: 전부 v1.1 §5.5 그대로.
- wall_ts 5분 이상 미래 = 로그 경고(클램프 금지). inline 콘텐츠 CID 불일치 = 프로토콜 위반(폐기+로그, 반복 시 연결 종료).
- 버전 협상 실패: `Error(VersionIncompatible)` → UI "앱 업데이트 필요"(진단 패널에 양측 버전 표시).
- KeyStale(자기 epoch < 현재): 발행 중단 + 수신 보류, wrap 수신 시 자동 복귀 — 구 epoch로의 송신은 하지 않음.

### 5.6 기본 파라미터 표

| 파라미터 | 값 | v1.1 대비 |
|---|---|---|
| PROTO_MIN/MAX | 2 | 1→2 (variant 변경 규칙) |
| 프레이밍 / max_frame | LengthDelimitedCodec u32 LE / 256KiB (Guest 32KiB) | 유지 + Guest 상한 신설 |
| INLINE_THRESHOLD | 32KiB (텍스트, E2E blob 내부) | 유지 |
| CHUNK_SIZE | 64KiB (암호문 기준) | 유지 |
| MAX_CONTENT_SIZE / READ_HARD_LIMIT | 10MiB(설정 1–100MiB) / 32MiB | 유지 |
| zstd 임계 | 4KiB 초과 텍스트, **암호화 전** | 순서 명시 신설 |
| debounce / 송신 캐시 | 150ms / 5개·5분 TTL(만료 Gone) | 유지 |
| heartbeat | idle 15s Ping, 45s 사망 판정 / 서버 세션 정리 90s | 유지 + 서버측 신설 |
| 재연결 백오프 | 1s→×2→**상한 30s**, jitter ±20%, D24 리셋 | 60s→30s (조정 사항 4) |
| signal rate limit | 클라당 10건/s — **서버 강제** + 클라 자율 | 강제 지점 이동 |
| fetch 타임아웃 / 동시성 | 10s(첫·청크 간) / 클라 1, origin 업로드 1(서버 티잉) | 유지 |
| 업로드 큐 | 보유자→서버 1회 업로드 bounded(32청크) — cache-through, backpressure는 이 구간에만(§5.4) | 재정의(라이브 티잉 폐기) |
| HEAD_CACHE_DEPTH / CONTENT_CACHE_BYTES | signal 4건 / **head signal별 본문 캐시**(CID 키), 총량 상한 ≤4×max_content_bytes(LRU 축출), 메모리 전용(D29), epoch 전환 시 무효화 | "본문 최신 1건" 재정의(§5.4, D31 정합) |
| 초대 코드 / TTL | Crockford Base32 12자(60-bit), 4-4-4 / 기본 1h·최대 24h | 6자리→12자 (D27, §4.3.6) |
| Argon2id | m=64MiB, t=3, p=1 (~0.5s) | 신설 |
| 초대 blob / mailbox | blob ≤ 4KiB / 멤버별 최신 KeyUpdate 1개 | 신설 |
| epoch grace | manual/periodic 10분, revoke 0 | 신설 (§4.4) |
| Guest lane | IP당 동시 1, 전체 8, TTL 60s, 신규 3/분, **세션·IP당 GetInviteBlob 5회**, GetLog는 유효 locator 소지 후만, AppendEntry(Add)는 미소비 초대 locator 바인딩(초대당 1건) | 페어링 창 정책 계승 + admission 게이트(§4.5) |
| nonce | XChaCha20-Poly1305 24B 랜덤 전치 | E2E 신설 |

---

## 6. 플랫폼별 클립보드 전략 (v1.1 §6 승계 — 무변경)

| 플랫폼 | 감지 | 방식 | 비고 |
|---|---|---|---|
| Windows | 완전 이벤트 | 메시지 전용 히든 윈도우 + `AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE` (clipboard-rs watcher) | Win11 알림 직후 데이터 미준비 → 50–100ms 지연 후 **동일 OpenClipboard 세션에서 포맷 목록+concealed 마커+데이터를 원자적으로 읽기**(§4.6). 쓰기 시 제외 포맷(D16) 동시 기록 |
| macOS | changeCount 워칭 | `NSPasteboard.general.changeCount` 정수 비교, 기본 500ms + timer tolerance(coalescing) | 순수 이벤트 API 부재(확정) — D8 요구사항 재정의. 화면 잠금/슬립 시 정지, 동기화 off 시 워처 완전 정지. TIFF→PNG 정규화 |
| Linux X11 | 이벤트(방식 M1 확인) | XFixes `SelectionNotify` — **단 clipboard-rs의 X11 watcher는 콘텐츠 해시 폴링일 가능성이 큼**(진짜 XFixes 이벤트 감시가 아님). M1에서 노출 방식 확인, 폴링뿐이면 **x11rb 기반 XFixes watcher를 별도 구현**(M1 범위·기간 계상 — Wayland 커스텀 코드와 유사하되 별개 항목) | 이벤트 소유자 윈도우 == 자신이면 무시(루프 방지 보조). lazy target 재시도 |
| Linux Wayland (KWin/wlroots — **Sway/KDE**) | 완전 이벤트 | `ext-data-control-v1` 우선, `zwlr_data_control_v1` 폴백 — **wayland-client 0.31 + wayland-protocols 직접 구현, wl-clipboard-rs(MIT/Apache-2.0)의 data-control 세션 코드를 감시 루프로 개작**(D7·D20), I/O는 wl-clipboard-rs 0.9 | KWin 6.6·Sway 1.11이 data-control 지원. 감시·I/O 모두 이 경로 |
| Linux Wayland (**GNOME/Mutter — 버전 무관 전 버전**) | **data-control 불가군 — 폴백 필수** | GNOME/Mutter는 프라이버시 정책상 wlr-data-control 및 후속 ext-data-control을 **의도적으로 미구현**(Mutter 이슈 #524 여전히 open, 2026 기준; Mutter !320 "클립보드 매니저"는 내부 지속성 기능이지 외부 클라이언트용 data-control 노출이 아님). 감시(wl-clipboard-rs data-control watcher)·I/O(wl-clipboard-rs가 ext/wlr-data-control 사용) **둘 다 GNOME 전 버전에서 동작 불가** — ≤48만이 아님. 지원 경로 = (a) **XWayland(X11+XFixes) 브리지** 또는 (b) **opt-in 저주기 폴링**(기본 off·사유 표기) 또는 (c) 코어 프로토콜 focus-steal 중 하나를 **1급 지원 경로로 문서화** | 사내 주력(GNOME/LTS) 가능성이 높아 폴백이 예외가 아니라 사실상 **주경로**인 실질 리스크 구간(R2). 한계: (a) XWayland 장기 제거 추세(GNOME 50+)로 영구 의존 금지, (b) 백그라운드 감시 제약. **"GNOME 49면 data-control 해결" 전제는 사실 오류이므로 폐기** — 계획은 data-control 지원 존재를 가정하지 않는다. M1에서 현재 Mutter 상태만 재확인 |

**포맷 매트릭스 (read 수용 → 정규형 / write 동시 기록)**

| 플랫폼 | read → 정규형(UTF-8 텍스트 / PNG) | write 동시 기록 |
|---|---|---|
| Windows | `CF_UNICODETEXT`→UTF-8(개행 원문 보존); 이미지: 등록 포맷 `"PNG"` 우선 → `CF_DIBV5` → `CF_DIB` → PNG 변환(clipboard-rs 내장 여부 M1 검증, 미비 시 image 크레이트) | `CF_UNICODETEXT`; `"PNG"` + `CF_DIBV5`(+`CF_DIB`); + 제외 포맷(D16) |
| macOS | `public.utf8-plain-text`; TIFF→PNG | `public.utf8-plain-text`; PNG(TIFF는 OS 자동 제공) |
| X11 | `UTF8_STRING` 우선, `STRING`(Latin-1)→UTF-8; `image/png` | `UTF8_STRING`+`STRING`; `image/png` |
| Wayland | `text/plain;charset=utf-8`; `image/png` | 동일 |

- 공통 추상화: `sb-clipboard`의 `ClipboardWatcher` / `ClipboardIo` trait + 런타임 백엔드 선택, mock 클립보드 headless 테스트 — v1.1 그대로.
- 감시 이벤트 수신 시에도 signal→fetch 원칙 적용: changeCount/시퀀스 번호 + 포맷 목록·크기 1차 판별 후에만 실제 read(READ_HARD_LIMIT 사전 차단 — §3.3).
- 소유권 유지(X11/Wayland): 쓰기 후 데이터 요청 응답 스레드 상주(트레이 상주형이라 자연 충족).

---

## 7. 저장소/히스토리/설정

### 7.1 클라이언트 레이아웃 (파일 0600, 디렉터리 0700; Windows는 %LOCALAPPDATA% 기본 ACL)

```
config_dir/settings.json          # atomic write (tempfile persist → rename)
data_dir/identity.json            # 공개 부분만 (secret은 keychain — 폴백 사다리 §4.2)
data_dir/workspace.json           # [개정] 구 peers.json 대체: {server_addr, server_fp,
                                  #   로그 엔트리 캐시(바이트열), last_head(seq,hash), epoch,
                                  #   멤버 표시 캐시(name/platform/last_seen)} + mac 필드(D22)
data_dir/history.db (+wal/shm)    # rusqlite bundled, WAL, secure_delete=ON
data_dir/logs/                    # tracing 파일 로그, 일 단위 회전·7일 보존 (§4.7)
```

- Windows Roaming 금지(Local 사용) — v1.1 그대로.
- **workspace.json 무결성(D22 개정)**: 캐노니컬 직렬화 위에 전용 MAC 키(keychain 상시 provisioning)로 HMAC-SHA256 태그. 로드 시 검증 실패 → `.quarantine` 격리 + 서버에서 로그 재취득·전체 체인 재검증(체인 서명 덕에 재조인 불필요 — v1.1의 "전 피어 재페어링"보다 완화). last_head·epoch 단조성 기록이 로그 롤백 거부(§4.3.1)의 근거.

### 7.2 히스토리 스키마 (v1.1 §7.2 승계 — 무변경 요지)

```sql
CREATE TABLE history (
  id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL,
  workspace_id BLOB NOT NULL,               -- [신설] 워크스페이스 소속 태깅(A→B 전환 시 혼입 방지, Major)
  kind TEXT CHECK (kind IN ('text','image')), origin TEXT NOT NULL,  -- 'local' | device_id
  dedup_mac BLOB NOT NULL UNIQUE,           -- keyed BLAKE3(dedup 키, content) — D21
  size_bytes INTEGER NOT NULL, pinned INTEGER DEFAULT 0,
  preview_nonce BLOB, preview_ct BLOB,      -- 암호화: 텍스트 앞 256자 / PNG 썸네일(긴변 256px)
  body_nonce BLOB, body_ct BLOB             -- 암호화 원본 (AAD = id||kind||created_at). NULL = 강등
);
```

- 썸네일 PNG / suppress는 인메모리 keyed CID·DB dedup은 별도 keyed_hash / 기본값(인메모리 30, 영속 off, 200개·7일, 이미지 ≤5MB·최근 20) / 인메모리 검색 / 정리 주기 / rusqlite 0.38 + rusqlite_migration 2 한 쌍 버전업: 전부 v1.1 그대로. epoch 회전으로 와이어 CID가 바뀌어도 DB dedup 키(로컬 전용)는 불변이므로 히스토리 dedup은 영향 없음.
- **워크스페이스 전환 시 혼입 방지(Major)**: `workspace_id`로 표시·검색을 현 워크스페이스로 필터링하고, `leave`/워크스페이스 전환 경로에 히스토리 crypto-erase를 결속(§8.6 위험 구역) — A→B 전환 후 A 시절 항목이 로컬에 섞여 남지 않게 한다. 서버 재클레임 등으로 `workspace_id`가 바뀌면 불일치 감지 → "워크스페이스가 변경/삭제됨" 오류 + 재온보딩 유도(§8.2).

### 7.3 settings.json v2 (v1.1 §7.3 대체)

```json
{
  "version": 2,
  "server":  { "address": null,
               "fingerprint": null },
  "workspace": { "name": null },
  "sync":    { "enabled": true, "sync_text": true, "sync_images": true,
               "max_content_bytes": 10485760, "auto_apply_received": true,
               "confirm_risky_content": true },
  "history": { "memory_max_items": 30, "persist_enabled": false, "max_items": 200,
               "retention_days": 7, "store_image_originals": true,
               "max_image_item_bytes": 5242880, "max_image_originals": 20 },
  "privacy": { "exclude_concealed": true,
               "excluded_apps": ["<동봉: 알려진 비밀번호 매니저 프로세스 목록>"],
               "exclude_patterns": [] },
  "key":     { "periodic_rotation_days": 0 },
  "device":  { "name_override": null },
  "app":     { "autostart": false, "start_minimized_to_tray": true,
               "notify_on_receive": true, "notify_on_member_join": true,
               "log_level": "info", "language": "system", "theme": "system" }
}
```

- **삭제**: `network` 섹션 전체 — `listen_port`(클라 리스너 폐지, D19)·`mdns_enabled`(D6 폐기)·`manual_peers`(수동 피어 폐기)·`interface`(리슨 소멸; 아웃바운드 LAN allowlist는 §4.5대로 코드 레벨). `device_name_override` → `device.name_override`. `app.notify_on_peer_request` → `notify_on_member_join`.
- **신설**: `server.address`("host:port"), `server.fingerprint`(SPKI SHA-256 hex — 온보딩 승인 시 고정, 변경 시 재확인 UI 강제), `workspace.name`(표시용 캐시), `key.periodic_rotation_days`(0 = off, 30/90 옵션 — §4.4).
- 그룹 키·자격 증명은 keychain(§4.2) — **설정 파일에 비밀 0** 원칙 유지. v1→v2 마이그레이션: `version` 기반, 구 `network.*` drop, 알 수 없는 키 보존(round-trip) 유지. 기본 장치명 비식별(`mac-3f7a`류) 유지.

### 7.4 sb-server 상태·설정 (신규 — D30)

**저장 (파일. tempfile→rename atomic write, SQLite 미도입)**

```
/var/lib/shareboard/  (0700, 파일 0600)
├─ identity.key / identity.crt    # 서버 cert — headless라 keychain 미적용, 파일 0600
├─ wslog.bin                      # 워크스페이스 로그 append-only(엔트리 바이트열 보존 — D28)
│                                 #  프레이밍 = [u32 길이][엔트리 바이트][32B 엔트리 해시] —
│                                 #  append 중 크래시 torn-write를 기동 시 tail 검증으로 감지,
│                                 #  마지막 온전 엔트리까지 트렁케이트 복구(state.json은 atomic write이나
│                                 #  wslog.bin은 append라 별도 필요). append는 write+fsync 후 성공 응답
└─ state.json                     # {version, claimed, setup_token_hash?,
                                  #  invites[{locator, blob_b64, expires_at, consumed}],
                                  #  mailbox[{to_fp, epoch, wrap_b64}]} — 전부 암호문/공개값
```

- 서버 디스크에는 **평문 콘텐츠·평문 해시·초대 코드·GK·평문 setup 토큰이 절대 저장되지 않는다**(blob·wrap은 전부 암호문, 로그는 서명된 공개 데이터, setup 토큰은 해시만). head 캐시는 메모리 전용(D29).
- **상태 수명·GC(Minor)**: 만료 `invites[]`·소비된 `mailbox` 항목은 기동 시 + 저빈도 타이머(예: 10분)로 **주기 GC**(`RevokeInvite`/`consumed`와 별개로 만료 항목 자동 청소). 로그 총 엔트리/바이트 상한 도달 시 신규 Add 거부 + 운영 경보, 체크포인트로 조인 검증 범위 축소(§4.5).
- **디스크 풀·append 실패(Major)**: append 실패 시 클라에 `Error(Busy)` 반환하고 **읽기 전용 모드로 전환**(기존 연결·fanout·GetLog는 유지, 신규 Add·PutInvite·PutKeyUpdate만 거부) + 운영 경보. `verify-log`/`status` CLI로 무결성·claimed·seq·head·연결 멤버를 오프라인 점검.
- 서버측 MAC은 **의도적 미도입**: 클라 D22는 MAC 키가 별도 격리 저장소(keychain)에 있어 의미가 있으나, 서버는 키가 파일 옆에 놓여 동일 침해 도메인 — 이득 없음. 서버 저장물 변조의 피해는 E2E 설계상 가용성/메타데이터로 한정됨(§4.1)을 위협 모델에 명시.

**server.toml (설정 파일 하나)**

```toml
bind_addr = "192.168.10.5"   # 필수. §4.5 allowlist 검증 — 사설/ULA/링크로컬 외 거부, 0.0.0.0/:: 금지
port = 45871                  # D19(개정) — 서버 단일 포트
data_dir = "/var/lib/shareboard"
log_level = "info"
setup_token_hash = "…"        # 관리자 오프라인 생성 토큰의 SHA-256(미클레임 시 필수) — 서버는 평문 미보유
# health_bind = "127.0.0.1:45872"   # 기본값. "" 로 비활성
# mdns_advertise = false            # 서버 자기 광고 opt-in (D6)
# max_connections = 64              # 초과 시 65번째는 즉시 Error(Busy) 거부(큐잉 없음)
# max_guest_connections = 8         # Guest 별도 예산 — Member 예산과 독립(스파이크가 Member 잠식 금지)
# max_content_bytes = 10485760
# content_cache_bytes = 41943040    # head 본문 캐시 총량 상한(≈4×max_content_bytes, §5.4)
```

- **max_connections 초과(Minor)**: Member/Guest **별도 예산**(예: 64/8)으로 관리하며 각 상한 초과 시 신규 연결은 큐잉 없이 즉시 `Error(Busy)` 거부. Guest 스파이크가 Member 슬롯을 잠식하지 못하도록 예산을 분리(§4.5).

**배포 (요지 — 산출물·절차는 §11 M6)**

- systemd: `DynamicUser=yes`, `StateDirectory=shareboard`, `Restart=on-failure`, `MemoryMax=128M`, `NoNewPrivileges` + `ProtectSystem=strict` + `ProtectHome` + `PrivateTmp`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, **`IPAddressDeny=any` + `IPAddressAllow=<사설 대역·링크로컬·ULA·localhost>`** — "사내망 외부 통신 0"을 커널 수준에서 이중 강제(코드 allowlist와 독립).
- Docker: musl 정적 빌드(rustls 순수 Rust) → `FROM scratch`/distroless(~10MB), `-p 45871:45871`, `HEALTHCHECK CMD ["/sb-server","healthcheck"]`(자기 /healthz 호출 — curl 불요), volume `/var/lib/shareboard`. mDNS 광고 필요 시 `--network host`/macvlan 문서화.
- `/healthz`(127.0.0.1:45872): `{"status":"ok","version","proto":[2,2],"uptime_s","members","online"}` — 인증 없음이므로 loopback 밖 바인딩 비권장.
- 리소스 상한: max_connections 64(guest 별도 8), RSS 유휴 <30MB·피크 <100MB(`MemoryMax=128M` 강제), tokio 2 workers·전 채널 bounded.
- **버전 혼재/업그레이드**: 게이트는 서버가 수행(지원 창 = [현재, 직전]). 절차 = ① 서버 먼저(구 클라 후방 호환 유지 의무) ② 클라이언트 순차. 서버 재시작은 자동 재연결 + head 재수렴으로 무설정 복구(계획 다운타임 수 초). state.json은 version 필드 + 알 수 없는 키 보존, 롤백 = 이전 바이너리 + 백업 복원.

**서버 백업/이전 (신규 — Major)**

| 파일 | 소실 시 영향 | 백업 |
|---|---|---|
| identity.key/crt | 지문 변경 → 전 클라 pinning 파손(수동 재확인 필요) | 필수 |
| wslog.bin | **admission 근거 소실 → 온라인 멤버도 전원 Guest lane 강등·동기화 불가. 프로토콜에 로그 재업로드 경로 없어 재구성 불가** | 필수(가장 치명) |
| state.json | 미배달 초대·mailbox 유실(재초대·재회전으로 회복) | 권장 |

- "identity.key만 주의" 서술은 오해를 부름 — **wslog.bin이 admission 근거**이므로 반드시 함께 백업.
- 이전 플레이북 2가지: ① **identity.key+crt+wslog.bin+state.json 그대로 복사** → 지문 유지·클라 무중단(권장). ② **신규 cert 생성** → SPKI 지문 변경 → 관리자가 새 지문 공지 → 기존 멤버가 §7.3 재확인 UI로 out-of-band 수동 재검증(신규 조인만 blob 검증이 적용되므로 기존 멤버는 이 경로 필수).
- (선택) wslog.bin 소실 시 **온라인 멤버 캐시 로그(workspace.json)로 재구성하는 restore 경로** 검토 — 다수 멤버 서명 로그가 동일 바이트열이라 복원 가능(M6 이후 과제).

**orphaned / 마지막-멤버 복구 (신규 — Critical)**

claimed 플래그가 서면 이후 다른 Genesis가 거부되므로, 다음 3경로가 복구 수단 없이 워크스페이스를 벽돌(brick)화한다: (a) 마지막 멤버 leave → 멤버 0 → 초대·조인 불가·새 Genesis도 거부, (b) 유일 멤버 기기 분실 → GK 소실 → 회전·admission·초대 불가, (c) 유일 멤버가 초대만 발급 후 오프라인/분실 → 조인자가 Add되었으나 GK를 영영 못 받는 '키 없는 반쪽 멤버'. 대응:
- **`sb-server reclaim` CLI**: claimed 해제 + 새 `setup_token_hash` 주입(명시 확인·기존 wslog 백업 유도) → 재클레임 허용. 최소한 wslog.bin·state.json **수동 삭제 재클레임 절차**를 §11 M6 배포 문서에 명문화.
- **UI 경고**: `leave_workspace`/멤버 제거 흐름에서 "이 동작으로 마지막 멤버가 되면 워크스페이스가 복구 불가하게 종료됨" 경고 + "나가기 전 다른 기기 초대" 유도(§8.6).
- **키 없는 반쪽 멤버 만료**: grant/Add 후 일정 시간 내 GK 미수신 시 해당 조인을 만료 처리(재초대 필요)하는 규칙 명시 — 로그에 Add된 채 영구 고착 방지.

---

## 8. UI/UX (v1.1 §8 대체 — 창 1개 + Svelte 5 + 트레이 상주는 유지)

Peers 탭·페어링 다이얼로그·mDNS 발견 UI는 전면 폐기. History 탭·고위험 확인 흐름(`clipboard-pending-confirm`)·트레이 메뉴 구성은 유지하되 트레이 상태 소스를 "피어 연결"→"서버 연결"로 교체.

### 8.1 화면 구성

워크스페이스 미구성(`onboarding_state != complete`)이면 창은 **온보딩 전용 화면**만 표시. 구성 후 좌측 탭 3개: **Members**(구 Peers 대체) / **History**(v1.1 그대로) / **Settings**(진단 패널 포함). 전 탭 공통으로 창 상단에 **서버 상태 배너**(§8.5) 고정.

### 8.2 온보딩

진입 화면: [새 워크스페이스 만들기](서버에 첫 접속) / [기존 워크스페이스 참여](초대 코드·URI).

**흐름 1 — 워크스페이스 생성 (최초 1대)**

| # | 화면 | UI | 백엔드 |
|---|---|---|---|
| C-1 | 서버 주소 입력 | `host:port`(기본 포트 45871 힌트) → [연결 확인] | `test_server_connection` — TCP→TLS, 서버 지문·버전 반환 |
| C-2 | 서버 지문 확인 | SPKI SHA-256 **전체 지문** 표시(절단 금지) + "관리자 공지 지문과 대조" → [이 서버 신뢰] | 승인 시 pinning 확정(TOFU + 수동 대조) |
| C-3 | setup 토큰·이름 | **관리자가 오프라인 전달한** setup 토큰, 워크스페이스 이름, 기기 이름(기본 비식별) → [만들기] | `create_workspace` — 토큰 해시 검증·Genesis 서명·업로드, **GK 로컬 생성**(서버 미전송) |
| C-4 | 완료 + 초대 유도 | [첫 초대 코드 발급] / [나중에] | 초대 다이얼로그(§8.3) 오픈 |

```
┌──────────────────────────────────────────────┐
│  서버 확인                          (2/3)    │
│  10.20.1.15:45871 · 서버 v0.9.2 · proto 2    │
│  서버 인증서 지문 (SHA-256)                  │
│  ┌────────────────────────────────────────┐  │
│  │ 4F2A 91C3 0B7E ... 전체 64 hex 표시    │  │
│  └────────────────────────────────────────┘  │
│  ⚠ 관리자가 공지한 지문과 반드시 대조하세요. │
│           [뒤로]        [이 서버 신뢰 →]     │
└──────────────────────────────────────────────┘
```

**흐름 2 — 참여 (초대 코드)**

| # | 화면 | UI | 백엔드 |
|---|---|---|---|
| J-1 | 초대 입력 | (a) 초대 URI 붙여넣기 1필드 — 즉시 `parse_invite_uri`로 주소·지문·코드 자동 채움 / (b) 수동: 서버 주소 + 코드 12자 → [참여] | URI 파싱은 Rust에서 |
| J-2 | 진행 표시 | 단계 체크리스트 순차 진행, 실패 시 사유 + [다시 시도] | `join_workspace` → `join-progress` 이벤트 |
| J-3 | 완료 | 워크스페이스 이름·멤버 수 확인 → [시작] | 탭 UI 전환, 동기화 활성 |

```
┌──────────────────────────────────────────────┐
│  워크스페이스 참여 중…                        │
│  ✔ 서버 연결 (TCP · TLS 1.3)                 │
│  ✔ 초대 코드 확인 (봉인 해제 · 서버 지문 검증)│
│  ✔ 워크스페이스 로그 검증                     │
│  ● 멤버 등록 중…                             │
│  ○ 그룹 키 수신                              │
│  ⓘ 그룹 키는 온라인 멤버가 자동 전달합니다   │
│                       [취소]                 │
└──────────────────────────────────────────────┘
```

주요 실패 문구(진단 가능한 사유 구분 — v1.1 원칙): `code_invalid_or_expired` "초대 코드가 틀렸거나 만료되었습니다. 새 코드를 요청하세요."(봉인 해제 실패 — 새 코드 필요 명시) / `no_member_online` "참여가 승인되었습니다 — 멤버가 온라인이 되면 자동으로 완료됩니다."(AwaitKey 대기, 취소 가능) / `fingerprint_mismatch`(수동 입력 경로: blob 내 지문과 불일치 = 진행 차단) / `version_incompatible` "서버(또는 앱) 업데이트 필요" + 양측 버전 표시 / `grant_reused` "이미 사용된 초대입니다" / `workspace_changed` "워크스페이스가 변경/삭제되었습니다 — 재온보딩이 필요합니다"(서버 재클레임으로 workspace_id 불일치).

- **창립자 지문 대조(Major)**: 조인 완료 전, UI는 서버 지문과 별도로 **창립자 지문(Genesis creator_spki SHA-256)**을 관리자 공지값과 대조하도록 안내한다 — blob이 창립자를 바인딩하나 "정당 창립 가장" 침해(§4.1)는 서버 지문 일치만으로 걸러지지 않으므로 창립자 지문을 out-of-band로 확인.
- **워크스페이스 전환/재클레임(Major)**: 이미 다른 워크스페이스 소속인 기기에서 참여 시 명시 확인 후 기존 leave(crypto-erase)를 선행(§3.4). 서버 재클레임으로 workspace_id가 바뀐 기존 클라는 Genesis/workspace_id 불일치를 감지해 `workspace_changed` 오류 + 재온보딩 유도(설정에 workspace_id 축약 표시로 동명 구별, §8.6).

### 8.3 초대 발급 다이얼로그 (모달 — 구 페어링 다이얼로그 대체)

Members 탭 [+ 새 기기 초대]·온보딩 C-4에서 진입. 발급 기기당 활성 초대 1개.

```
┌──────────────────────────────────────────────┐
│  새 기기 초대                          [×]   │
│         F7KQ - 2M9X - 4WD8                   │
│   (Crockford Base32 12자 — 혼동 문자 제외)   │
│   유효 시간  ⏱ 57:12 남음      [초대 취소]   │
│  [ 코드만 복사 ]  [ 초대 URI 복사 ]          │
│  초대 URI = 서버 주소+지문+코드 한 줄 링크.  │
│  사내 메신저 DM으로 전달하면 붙여넣기 한 번. │
│  ⚠ 1회용입니다. 사용/만료 시 무효.          │
│     발급 후 이 기기가 꺼져 있어도 되며,      │
│     멤버 중 1명만 온라인이면 참여가 완료됩니다│
└──────────────────────────────────────────────┘
```

- **초대 URI**: `shareboard://join?v=1&s=<host:port>&fp=<서버 SPKI SHA-256 hex>&c=<XXXX-XXXX-XXXX>` — 지문 동봉으로 수신자는 TOFU 확인 화면 없이 자동 검증(메신저 채널이 지문 전달 채널 겸용). 수동 폴백은 주소+코드만 — blob 내 지문으로 소급 검증되므로 안전성 동등(§4.3.4).
- 만료 → "만료됨" + [새 코드 발급], 60초 미만 시 타이머 경고색. 사용 완료(`invite-state-changed{used}`) → "✔ '<기기>'가 참여했습니다" + Members 실시간 반영. 조인은 전 멤버에게 알림(무단 조인 조기 발견 — §4.1).
- 초대 URI·코드는 로깅 금지 대상(§4.7). 유출 = TTL 내 1회 조인 시도 허용 — 전달은 사내 메신저 DM/구두 권장(배포 문서 명시).

### 8.4 Members 탭 (구 Peers 3분할 폐기 → 단일 목록)

```
┌─ Members ────────────────────────────────────────────┐
│ ● 서버 연결됨 · 10.20.1.15:45871 · "design-team"     │
│  내 기기: mac-3f7a (macOS) · 지문 4F2A…(클릭=전체)   │
│  epoch 4 · 로그 head #23                  [이름 변경] │
│──────────────────────────────────────────────────────│
│ 멤버 (4)                          [+ 새 기기 초대]   │
│  ● linux-9b21   Linux    온라인                 [⋮]  │
│  ● win-8c2d     Windows  온라인 · 방금 동기화   [⋮]  │
│  ○ mac-77e1     macOS    오프라인 · 2시간 전    [⋮]  │
│──────────────────────────────────────────────────────│
│ 활성 초대 1건 (52분 남음)                  [철회]     │
│ [⋮]: 전체 지문 보기 / 워크스페이스에서 제거          │
└──────────────────────────────────────────────────────┘
```

- 항목: 이름(E2E profile), 플랫폼, presence 온라인 배지, last_seen, 지문(축약+클릭 전체 — 절단만으로 신뢰 판단 금지), 신 epoch 수신 여부(회전 진행 표시용). 헤더에 **workspace_id 축약 표시**(동명 워크스페이스 구별 — Major)와 창립자 지문 표시. 피어별 sync 토글·수동 IP 추가 폐기(단일 보드 — 전역 sync만). **생성자도 Genesis 이후 일반 멤버**와 동일하게 [⋮]에서 제거 가능(유일 멤버일 때는 아래 orphan 경고 — Minor).
- **멤버 제거(revoke) 확인 다이얼로그**: "제거하면 그룹 키가 즉시 회전됩니다 · 온라인 기기는 자동 갱신 · 오프라인 기기는 다음 접속 시 갱신 · 제거된 기기는 이후 콘텐츠 복호 불가(제거 이전 유출은 되돌릴 수 없음)" → [취소] / [제거 및 키 회전]. **제거로 멤버가 0이 되는 경우** 추가 경고 "이 동작으로 워크스페이스가 복구 불가하게 종료됩니다"(§7.4 orphan). 회전 진행은 배너 "키 회전 중… (3/4 기기 갱신)"(`key-rotation` 이벤트).

### 8.5 서버 연결 상태 표시 (배너 + 트레이)

| 상태 | 배너 | 트레이 (v1.1 §8.2 자산 재사용) |
|---|---|---|
| connected | `● 서버 연결됨 · addr · 워크스페이스명` (1줄 무채색) | 정상 글리프, 전송 중에만 syncing 프레임(#38BDF8) |
| reconnecting | `◌ 재연결 중… 다음 시도 12초 후 [지금 재시도]` (황색) | offline 표현(macOS 알파 40% / #94A3B8) |
| key_stale | `⚠ 키 갱신 대기 중 — 동기화 일시 불가` (황색) | offline 표현 |
| unconfigured | `⚠ 서버가 설정되지 않았습니다 [설정 열기]` | 경고 배지(!) — 클릭 시 온보딩 |
| paused | v1.1 그대로(∥ 배지, #F59E0B) — 서버 상태와 직교 | 유지 |

트레이 메뉴는 v1.1 그대로(동기화 on/off, 창 열기, 15/60분 일시정지, 히스토리 전체 삭제, 종료) + 상단에 서버 상태 1줄 비활성 라벨.

### 8.6 Settings 탭 + 진단 패널

- 섹션: ① 서버 — 주소(읽기전용 + [변경]은 재확인 다이얼로그 경유), pinned 지문 전체 표시, 워크스페이스 이름 + **workspace_id 축약**(동명 구별) + 창립자 지문 ② 동기화/개인정보/히스토리 — v1.1 그대로 ③ 키 — 수동 [키 회전], 주기 회전(off/30/90일), 현재 epoch ④ 기기 — 이름, keychain/폴백 모드 표기(평문 폴백 상시 경고), Wayland 제한 사유 ⑤ 앱 — autostart·알림·로그 레벨 ⑥ 위험 구역 — [워크스페이스 나가기](로컬 키·히스토리 crypto-erase + Leave — 남은 멤버 측 회전 권고 안내; **내가 마지막 멤버면 "워크스페이스가 복구 불가하게 종료됨" 경고 + 나가기 전 다른 기기 초대 유도**, §7.4), [앱 초기화].
- **진단 패널**: 앱·프로토콜 버전 + 서버 버전·프로토콜 병기 / **서버 연결 테스트** — ① TCP 도달 → ② TLS 1.3 → ③ 지문 pinning 일치 → ④ 멤버 인증 단계별 성공/실패 + 사유 코드(Timeout/Refused/FingerprintMismatch/AuthRejected/VersionMismatch), 임의 주소 테스트 허용(서버 이전 사전 점검) / 현재 세션 정보(연결 시각·재연결 횟수·마지막 단절 사유) / 로그 번들 내보내기(§4.7 — 초대 코드·콘텐츠 불포함).

---

## 9. 아이콘/브랜딩 (v1.1 §9 승계 — 무변경)

- **채택: C안 "Share Nodes"** — 클립보드 + 공유 노드 글리프. 32px 실측 렌더 검증에서 식별성 최고.
- 팔레트: primary `#2563EB`, glyph `#FFFFFF`, 상태색 syncing `#38BDF8` / connected `#22C55E` / paused `#F59E0B` / offline `#94A3B8`.

**마스터 SVG (1024px, `assets/icons/app-icon.svg`)**:
```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <rect x="64" y="64" width="896" height="896" rx="196" fill="#2563EB"/>
  <rect x="272" y="200" width="480" height="656" rx="68" fill="none" stroke="#FFFFFF" stroke-width="52"/>
  <rect x="422" y="144" width="180" height="116" rx="40" fill="#2563EB" stroke="#FFFFFF" stroke-width="44"/>
  <line x1="416" y1="560" x2="626" y2="424" stroke="#FFFFFF" stroke-width="46"/>
  <line x1="416" y1="560" x2="626" y2="696" stroke="#FFFFFF" stroke-width="46"/>
  <circle cx="416" cy="560" r="58" fill="#FFFFFF"/>
  <circle cx="626" cy="424" r="58" fill="#FFFFFF"/>
  <circle cx="626" cy="696" r="58" fill="#FFFFFF"/>
</svg>
```

- 트레이는 전용 글리프(클립보드+⇄, 순수 검정+알파, 16/22px 검증 완료 — `tray-template.svg`), macOS `icon_as_template: true`.
- 파이프라인: `resvg`로 SVG→1024 PNG → `cargo tauri icon` → 상태별 트레이 PNG를 `src-tauri/icons/tray/` 생성, 전 과정 `scripts/gen-icons.sh` 고정, `include_bytes!` 내장·런타임 `set_icon` 교체만 — v1.1 그대로.

---

## 10. 프로젝트 구조

```
shareboard/
├─ src/                            # 프론트 (Svelte 5 + Vite + TS)
│  ├─ main.ts / App.svelte
│  ├─ lib/
│  │  ├─ ipc.ts                    # invoke/listen 타입 안전 단일 진입점
│  │  ├─ stores/                   # members.svelte.ts, history.svelte.ts, settings.svelte.ts, server.svelte.ts
│  │  └─ components/               # MemberList, InviteDialog, OnboardingFlow(Create/Join),
│  │                               #   ServerStatusBanner, HistoryList, SettingsPanel, DiagnosticsPanel
│  │                               #   (PeerList·PairingDialog 삭제)
│  └─ styles/
├─ src-tauri/
│  ├─ Cargo.toml                   # workspace root
│  ├─ tauri.conf.json (+ tauri.conf.dev.json — dev CSP 완화 merge)
│  ├─ capabilities/main.json
│  ├─ icons/  /  icons/tray/
│  ├─ src/
│  │  ├─ main.rs / lib.rs          # Builder 조립, setup에서 core spawn
│  │  ├─ tray.rs / commands.rs / events.rs   # 얇은 어댑터 — Tauri 타입은 밖으로 못 나감
│  └─ crates/                      # UI 독립 코어 (workspace members)
│     ├─ sb-core/                  # 도메인 타입, sync engine, LWW/suppress, 설정
│     ├─ sb-clipboard/             # Watcher/Io trait + win/x11/wayland/mac 백엔드, concealed, 포맷 매트릭스
│     ├─ sb-proto/                 # [신규] C2s/S2c·SignalHdr·LogEntry, CBOR, 프레이밍 상수,
│     │                            #   주소 allowlist — sb-net과 sb-server가 공동 의존
│     ├─ sb-net/                   # 서버 세션(TLS 다이얼·재연결 FSM·프레이밍) — 리스너/mdns 소멸
│     ├─ sb-crypto/                # identity/KEM 키, GK·epoch·HKDF 파생, 로그 체인 서명/검증,
│     │                            #   초대 봉인/개봉(Argon2id·grant), KeyUpdate wrap, workspace 캐시 MAC
│     ├─ sb-store/                 # rusqlite 히스토리(필드 암호화, keyed dedup), JSON 영속화, keyring
│     ├─ sb-platform/              # D24: wake/네트워크 변경 감지 플랫폼 바인딩
│     └─ sb-server/                # [신규] headless 서버 바이너리 (§3.2 — 라이브러리 구조 + 얇은 main,
│                                  #   in-process 기동 가능 → §13.2 통합 테스트)
├─ assets/icons/                   # SVG 마스터 (app-icon.svg, tray-template.svg)
├─ deploy/                         # [신규] server.toml 예시, shareboard-server.service,
│                                  #   Dockerfile, docker-compose.yml, 서버 운영 문서
├─ scripts/gen-icons.sh
├─ .github/workflows/build.yml     # 3-OS 매트릭스 + 서버 빌드·Docker job (§13.4)
└─ package.json
```

의존 방향: `UI → (IPC) → src-tauri 어댑터 → sb-core → {sb-clipboard, sb-net(→sb-proto, sb-crypto), sb-store, sb-platform}`, `sb-server → {sb-proto, sb-crypto(검증부)}`. sb-* 상호 직접 의존 금지(명시 예외만). 코어·서버 모두 CLI/in-process 단독 구동 가능(통합 테스트용).

**주요 크레이트 (버전 고정)**: tauri 2.11 / tauri-plugin-autostart 2.x / tokio 1.53 / tokio-util 0.7(codec) / rustls 0.23 / tokio-rustls 0.26 / rcgen 0.14 / **p256 0.13(ECDSA 로그 서명 — D4)** / **x25519-dalek 2(KEM — D4)** / hkdf 0.12 / hmac 0.12 / sha2 0.10 / chacha20poly1305 0.10 / blake3 1.8(keyed) / **argon2 0.5(초대 코드 파생 m=64MiB — D27, 파일 폴백 암호화 겸용)** / zeroize 1.8 / secrecy 0.10 / keyring 3 / serde 1 / ciborium 0.2 / serde_json 1 / **toml(sb-server 설정)** / clipboard-rs 0.3 / arboard 3.6(feature flag) / wl-clipboard-rs 0.9 / wayland-client 0.31 + wayland-protocols / rusqlite 0.38(bundled) + rusqlite_migration 2(한 쌍 버전업) / png 0.18 / image 0.25 / zstd 0.13 / if-addrs 0.13 / tracing 0.1 + tracing-subscriber 0.3 + tracing-appender 0.2 / objc2-app-kit·windows-sys·zbus 5+rtnetlink(D24) / dirs 6 / tempfile 3 / rand(OsRng).
**제거**: spake2(D3 폐기 — 미감사 리스크 소거), mdns-sd(D6 폐기 — 서버 광고 opt-in 구현 시 sb-server 전용 의존으로만 재도입). **금지 유지**: bincode·iroh·GPL 크레이트(D20).

---

## 11. 마일스톤

| # | 이름 | 범위 | 완료 기준 | 기간 |
|---|---|---|---|---|
| **M1** | 스켈레톤 + CI + 클립보드 감시 *(대부분 유지)* | workspace 구성(sb-* + Tauri + **sb-proto/sb-server 스켈레톤 신규** — server.toml 로드·LAN 바인딩·headless 기동/종료만), 최소 트레이(아이콘+Quit), **CI: 3-OS 매트릭스(ubuntu-latest+`container: ubuntu:22.04`/macos/windows) + 서버 Linux 빌드·Docker 이미지 빌드 job 추가**, 로깅 기반(tracing, §4.7 — 서버 동일 원칙), `ClipboardWatcher` trait + Win/X11/macOS 백엔드, **Wayland/X11 스파이크(1주 분리 배정)**: wl-clipboard-rs 개작 data-control watcher를 **KWin/Sway에서 실검증**(GNOME은 §6대로 **data-control 불가군 — 지원 존재 가정 금지**), **GNOME은 폴백 경로(XWayland 브리지 / opt-in 폴링) 실검증** + 현재 Mutter #524 상태만 재확인, **X11 watcher가 XFixes 이벤트인지 해시 폴링인지 확인**(폴링뿐이면 x11rb XFixes watcher 구현을 M1 범위·기간에 계상), Windows DIB↔PNG 내장 여부 검증, 사내 표준 데스크톱/GNOME 실사, **사내 서버 호스트 확보 협의 개시(N2 — M2 착수 전 결론)** | 3-OS CI 그린 + 서버 바이너리·Docker 이미지 빌드 그린. 각 OS에서 텍스트 복사 시 즉시 keyed 해시 로그, 유휴 CPU ~0% 실측. Wayland 지원 매트릭스 문서 확정(**GNOME = 폴백 경로 실검증, data-control 미가정**). 서버 호스트/운영 주체 결정 문서화 | 3주 |
| **M2** | 최소 릴레이 서버 + 평문 텍스트 동기화 | sb-server: TCP listener + LengthDelimitedCodec + ciborium(sb-proto 공유), 세션 관리, **ClipSignal fanout(발신자 미반송)**, inline 텍스트 중계, **head 캐시(메모리)** — 재접속/late-join 수렴, **주소 allowlist(§4.5) — 서버 bind·accept, 클라 아웃바운드는 설정된 서버 주소만**. 클라: 서버 주소 설정·재연결 백오프(1s→×2→30s)·heartbeat, 에코 방어(suppress set + 선택 LRU — 단일 홉이라 다단 루프 구조적 부재), LWW key. **TLS·E2E 삽입 자리를 프레이밍에 미리 설계(M3에서 교체만)** | 맥 로컬 서버 + loopback 클라 3개에서 3자 텍스트 동기화 수렴, Linux VM 클라 1대 추가(**NAT VM 가능, bridged 불요**)에서 양방향 확인, 연속 복사 루프 미발생, 서버 재시작 시 전 클라 자동 재접속 + head 재수렴, 전역 IPv4/IPv6·비-allowlist 주소 거부 확인 | 2주 |
| **M3a** | TLS + 워크스페이스 로그 + 초대 조인 | **TLS**: rustls TLS 1.3, 서버 자기서명 cert(첫 기동 생성)·클라 지문 pinning(초대 blob 동봉 + 생성자 TOFU·전체 지문 UI + 창립자 지문 대조), 클라 identity(rcgen+keyring, 폴백 사다리) mTLS — admission은 로그 도출, 비멤버 = Guest lane 제한. **워크스페이스 로그(D28)**: Genesis/Add/Remove/Epoch 서명 체인(+RotateKem), 서버 append 직렬화(torn-write 복구·형식 사전검증)·클라 전체 검증(규칙 ①~⑦·포크/스킵), setup_token_hash 부트스트랩, workspace.json MAC(D22). **초대 조인(D27)**: 12자 코드→Argon2id locator/K_seal, grant 키·blob PUT/GET, Guest admission 게이트(Add=미소비 locator 바인딩), 조인(blob 복호→로그 검증→Add), TTL·철회 | (1) 초대 코드 조인·Add 성공 (2) 오타 코드 실패(AEAD)·TTL 만료 거부·**동일 grant 중복 Add 거부**·미소비 locator 없는 Add 거부 (3) 비멤버 핸드셰이크 거부 (4) 로그 롤백·torn-write 복구·member_set_hash 검증 | 4주 |
| **M3b** | 그룹 키 E2E + 제거·회전 | **그룹 키 E2E(D26·D31)**: GK·HKDF 파생·SignalBody 봉인·AAD 결합 — 서버는 SignalHdr{id, epoch, ct_size}+암호문만, **서버는 코드·GK를 알 수 없음**. GK wrap 전달(KEM pk), split-view 방어(presence head 교차검증 필수·enc_profile 단조 바인딩). **제거+회전(원자 플로우)**: Remove+Epoch(member_set_hash/wrapped_bundle_hash)+wrap 배포(mailbox — 오프라인 큐잉, entry-hash 바인딩), epoch 격리(revoke grace 0), 이행불가 Epoch/경합 회복 | (1) GK E2E 수신·양방향 (2) Wireshark 평문 미노출 + **서버 저장·로깅 산출물 전체에 평문·GK 부재**(§13.2) (3) 제거→회전 후 제거 기기 복호 불가·재접속 거부·grace 0 (4) 위조 Revoked 무시(Remove(self) 부재) (5) 보안 회귀 스위트 통과 | 4주 |
| **M4** | 이미지 + 히스토리 *(대부분 유지)* | PNG 감지/청크 전송 — **암호문 청크 서버 중계**(캐시 상한·초과 `Gone`), 10MiB 상한·Supersede/Abort, READ_HARD_LIMIT·사전 크기 판별(§3.3), Windows 포맷 매트릭스(DIB↔PNG), rusqlite 암호화 히스토리(opt-in, keyed dedup D21, crypto-erase), concealed 필터(동일 세션 원자), 재복사 API, GNOME ≤48 XWayland 폴백 실검증 | 맥 스크린샷 → Win/Linux 붙여넣기 + Windows DIB 왕복. 히스토리 복원. 비밀번호 매니저 항목 미전파. 초대형 이미지(100MB급) RSS 유지·스킵. **서버 캐시에 암호문만 존재 확인** | 1.5주 |
| **M5** | UI 완성 + 트레이 고도화 | **Peers 탭 → Members 탭**(멤버 목록·last_seen·초대 다이얼로그(12자 코드+TTL 카운트다운)·제거[=회전 포함 단일 동작]·수동 회전), **온보딩**(생성 vs 서버 주소+코드 조인, §8.2), Settings 서버 주소·지문 표시, **진단 패널 개정**(서버 연결 단계별 테스트·양측 버전·로그 번들), History·고위험 확인 흐름·트레이(서버 오프라인·key_stale 상태 추가)·아이콘 파이프라인·autostart. M4와 병행 가능 | 터미널 없이 UI만으로 생성→초대 발급→타 기기 조인→동기화→히스토리→멤버 제거(회전) 전 과정 시연, 진단 패널로 실패 사유 구분, 3-OS 트레이 확인 | 2주 |
| **M6** | 패키징/배포 (클라이언트 + 서버) | 클라: dmg/NSIS(+MSI)/deb·rpm, 의존성 명시, 서명 절차 — 유지. **서버 배포물 신규**: 바이너리(tar.gz, gnu+musl static) + systemd unit + Dockerfile/compose + server.toml 레퍼런스 + `fingerprint`/`setup-token` CLI 안내, 사내망 외부 통신 0 확인 절차. 배포 문서 개정: **방화벽 = 서버 호스트 인바운드 TCP 1포트(45871)만 — 클라 인바운드·UDP 5353 삭제**, 서버 백업/이전 절차(**백업 필수 = identity.key/crt + wslog.bin + state.json**, wslog.bin 소실 = admission 근거 소실·복원 불가 명시, 지문 유지 vs 신규 cert 재확인 2 플레이북 — §7.4), **orphaned/마지막-멤버 복구(`sb-server reclaim` 또는 수동 파일 삭제 재클레임 절차)**, `verify-log`/`status` 진단·알림 런북(재시작 루프·admission 실패율·디스크 풀), **업데이트: 서버 먼저(proto v·v-1 동시 수용) → 클라 순차**, 롤백 방침, 주기 회전 권장(§4.1 FS) | 클린 서버 호스트(systemd·Docker 각 1회)에 문서만으로 설치→클라 3-OS 클린 설치→조인→동기화 성공. §13.3 전항목 통과 | 1.5주 |

**일정 재기준(Major).** 이전 초안의 "12.5주 + 버퍼 = 15–16주"는 저평가였다: M3 한 마일스톤에 묶였던 TLS·로그체인·초대조인·E2E 그룹키·제거/회전은 각각 독립 테스트 스위트가 필요한 다주 보안 서브시스템이라 **M3a(4주)+M3b(4주)로 분할**했고, 위 표의 per-row 수치는 서브시스템별 낙관 추정이다. **단일 개발자 현실 총 일정 ≈ 30~45주**(암호/보안 서브시스템의 통합·회귀·플랫폼 실검증 포함)로 재기준하며, 단축이 필요하면 **2~3인 인력을 명시 배정**한다(예: 서버/암호 1인 + 클라이언트/플랫폼 1인 + UI 1인). **15–16주 수치는 대외 커밋 숫자로 사용하지 않는다**(버퍼 포함 수치 자체가 저평가). 코드사이닝 인증서는 **M3a 시점 착수**. 사내 서버 호스트 확보(N2)는 **M1 중 결론** — 미확보여도 M2~M3는 맥 로컬 + Docker로 무차단 진행.

---

## 12. 리스크와 완화책

**소멸·격하된 기존 리스크**

| 구# | 리스크 | 처리 | 근거 |
|---|---|---|---|
| R4 | 사내망 mDNS 차단 | **소멸** | 탐색 자체가 없음 — 고정 서버 주소:포트 1개(D6 폐기). 잔여(주소 오입력·DNS)는 진단 패널 연결 테스트로 흡수. 부수 개선: AP client isolation·VLAN 분리 환경에서도 서버만 도달하면 동작 |
| R6 | 에코 루프 | **격하(하)** | 서버 발신자 미반송 + 클라 재발행 금지 → 다단 루프 구조적 부재(§5.3). OS 바이트 변형·Windows 다중 이벤트의 자기 에코만 잔존(suppress set + PNG 정규화, M2 완료 기준 유지) |
| — | 페어링 관련(병렬 추측·세션 직렬화 등) | **소멸** | 페어링 프로토콜 폐기(D3). 코드 관련 면은 N3으로 이관 |

**신규 리스크**

| # | 리스크 | 확률/영향 | 완화책 |
|---|---|---|---|
| N1 | **서버 SPOF** — 서버 다운 시 전체 동기화 정지 | 중/상 | systemd `Restart`/Docker restart policy, 서버를 stateless에 가깝게(로그 파일+state.json+메모리 캐시 — 재기동 수 초), 클라 무한 백오프 재접속 + head 재수렴(M2 완료 기준), 오프라인 중 로컬 기능(히스토리·복사) 정상, 트레이 오프라인 표시. **이중화/HA는 명시적 비목표** |
| N2 | **서버 운영 주체/호스트 확보 실패·지연** | 중/중 | 요구 사양 최소화(단일 바이너리·RSS 수십 MB·설정 파일 1개)로 IT 협의 문턱 낮춤, systemd/Docker 양쪽 지원, M1 중 협의 개시·결론, 개발은 맥 로컬 서버로 비차단 |
| N3 | **초대 코드 유출** — 탈취자의 워크스페이스 조인 | 중/상 | 60-bit + Argon2id + **blob에 GK 미포함(D27)** + TTL(기본 1h)로 대입은 무의미화(§4.3.6), 1회성은 체인 규칙으로 서버 신뢰 없이 보증, 발급 UI 철회, **조인 성공 시 전 멤버 알림**(무단 조인 조기 발견), 의심 시 revoke = 즉시 필수 회전(N4)으로 사후 무력화 |
| N4 | **그룹 키 회전 누락/실패** — 제거된 멤버가 계속 복호 | 중/상 | 제거와 회전을 **단일 원자 플로우 강제**(회전 없는 제거 동작 자체가 없음 — D26), epoch 격리(revoke grace 0), 오프라인 멤버 wrap mailbox(암호문만), 회전 완료/미완 표시(Members 탭), §13.2 회귀 테스트 상시 검증 |
| N5 | **서버-클라 버전 혼재** — 프로토콜 비호환 | 중/중 | 버전 게이트를 서버가 수행(지원 창 = 직전 1개), **서버 먼저 업데이트 규칙** 배포 문서 명시, 진단 패널 양측 버전 표시, 비호환 시 명시적 오류 + UI 안내 |

**유지되는 기존 리스크**

| # | 리스크 | 확률/영향 | 완화책 (변경점만 주석) |
|---|---|---|---|
| R1 | macOS 이벤트 API 부재 | 확실/중 | 변경 없음 — D8 changeCount 워칭 |
| R2 | GNOME Wayland 감시 제약 | **상(재평가↑)** | **정정**: GNOME/Mutter는 버전 무관 data-control 미구현(§6) — 폴백(XWayland 브리지/opt-in 폴링)이 예외가 아니라 GNOME **주경로**. M1은 data-control 지원 가정 없이 폴백 경로를 실검증하고, 배포 문서에 GNOME 백그라운드 감시 제약·XWayland 장기 제거 추세를 1급 항목으로 명시 |
| R3 | Linux 트레이 | 중/중 | 변경 없음 |
| R5 | 미서명 바이너리 마찰 | 중/중 | 변경 없음 — M3 착수. **서버는 headless라 서명 마찰 없음** |
| R7 | 절전 복귀/IP 변경 후 연결 사멸 | 중/**중→하** | **단순화**: 재연결 대상이 고정 서버 1개 — 리스너 재바인딩·mDNS 재등록 불필요, D24 감지 → 백오프 리셋 + 재다이얼만 |
| R8 | Windows 클립보드 경합/타이밍 | 중/중 | 변경 없음 |
| R9 | 일정 지연 연쇄 | 중/중 | Wayland 스파이크 분리·버퍼 유지. **서버 신규 코드는 M2~M3에 국한, sb-proto 공유로 중복 최소화** |
| R10 | ubuntu-22.04 러너 deprecation | 확실/중 | 변경 없음 — **서버 빌드·Docker job도 동일 컨테이너 전략** |

---

## 13. 테스트 전략

### 13.1 유닛 테스트 (sb-* + sb-server, CI 상시)

- 프로토콜: 프레임 round-trip(proptest)·잘린 프레임·과대 길이·미지 variant 거부 — 유지. **C2s/S2c(인증·signal·fetch 라우팅·epoch·로그 메시지) 추가**.
- **서버 릴레이 로직**: fanout 발신자 제외, **cache-through 후 fan-out**(요청자별 독립 커서·라이브 티잉 부재)·**head signal별 본문 캐시**·`Gone`·**보유 멤버 릴레이(②)**·`CONTENT_CACHE_BYTES` LRU, 세션 정리, admission(로그 도출) 판정, Guest lane 제한(허용 4종 외 즉시 종료·**GetLog 게이트**·**GetInviteBlob 5회 상한**·**Add 미소비 locator 바인딩**), **Member/Guest 예산 분리·초과 시 Busy**, 만료 invite/mailbox GC, 버전 협상 게이트.
- **로그 체인**: 검증 규칙 전체 — 해시 연결·seq 단조, **동일 grant_pk 중복 Add 거부**, 제거 멤버 서명 무효, grant 만료(±24h), epoch 단조(+1), **member_set_hash 독립 재계산 대조(규칙 ⑥)·wrapped_bundle_hash 대조(규칙 ⑦)**, **포크/오염 엔트리 스킵(최장 유효 접두)·Conflict 후 재append**, Genesis 고정·타 Genesis 거부, **torn-write tail 트렁케이트 복구**, 바이트열 보존 round-trip.
- 암호: GK AEAD round-trip·**AAD 변조 복호 실패(SignalHdr의 id/epoch/ct_size 및 origin 각각)**, HKDF 도메인 분리(블롭 교차 스플라이싱 거부), **로그/회전 서명 프레이밍이 TLS CertificateVerify 구조와 결코 충돌 불가(D4 도메인 분리)**, **초대 — 오타 코드 AEAD 실패·만료·위조 blob 거부, locator 파생 결정성, Argon2 파라미터 고정**, **회전 — 멤버별 wrap/unwrap, revoke 시 구 epoch 신호 즉시 거부(grace 0)·manual grace 10분, 제거 멤버 키로 신키 unwrap 불가, wrap의 epoch_entry_hash/member_set_hash 바인딩 검증(고아 Epoch wrap 미채택)·동시 회전 경합(Conflict→재발행)·이행불가 Epoch→e+2 대체 회복, KEM 회전(RotateKem) 후 구 sk unwrap 불가**, workspace.json MAC 무결성·변조 격리, **로그 롤백 거부(last_head 단조성)**, **enc_profile 단조 seq 후퇴 재생 거부**. *(폐기: SPAKE2 세션 직렬화·페어와이즈 trust store 테스트, prev_epoch_confirm 테스트 전체)*
- 네트워크: 주소 allowlist 판정 — 유지. **클라 아웃바운드가 설정된 서버 주소 외 전부 거부** 추가.
- 동기화: suppress 에코 억제, LWW 수렴(property test), 크기 상한, concealed 원자 검사, 고위험 패턴 — 유지. keyed CID 재계산 검증 추가.
- 저장: 암호화 round-trip, keyed dedup, retention, crypto-erase — 유지.
- 전제: 클립보드 mock + **sb-server를 in-process 라이브러리로 기동**(바이너리는 얇은 main) → 전체 headless.

### 13.2 통합 테스트 (1 서버 + 3 클라이언트, loopback)

- 한 테스트 바이너리에서 **서버 1개(in-process) + 코어 클라 노드 3개(mock 클립보드)** 기동 → setup 토큰으로 워크스페이스 생성 → 초대 코드로 2·3번 노드 조인(GK wrap 전달 포함) → 양방향 텍스트/이미지 → **서버 재시작 → 전 노드 자동 재접속·head 재수렴(재발행 경로)** → 동시 신호 경합(서로 다른 두 클라) → 이미지 fetch 중 더 높은 key 도착 시 Supersede/Abort → 전 노드 head 일치 → **멤버 제거 + 키 회전 → 잔존 2노드 정상 동기화 지속 + 제거 노드 배제**.
- **보안 회귀 스위트 (개정)**:
  1. **비멤버 인증 거부** — 미등록 identity cert는 Guest lane 4종 외 전부 거부, member 메시지 시도 시 즉시 종료.
  2. **초대**: 오타 코드 조인 실패(AEAD) + **동일 grant 2회째 Add 거부(서버 consumed 무시하고 체인 규칙만으로)** + TTL 만료 blob 거부 + 철회된 초대 거부 + **미소비 locator 없는 Guest AppendEntry(Add) 거부(로그 오염 스팸 차단)** + 오염 엔트리 후 정직 멤버 Conflict 회복(스킵·재append).
  3. **서버 blind 검증(콘텐츠 평문·GK 미보유)** — 3단: (a) 계측 빌드 서버가 수신·중계·캐시한 **전 바이트를 테스트 sink로 복제**해 평문 마커(테스트 전용 문자열/이미지 시그니처)·GK 바이트 미검출 단언, (b) 종료 후 **서버 data_dir·로그 전체 스캔** 마커 미검출, (c) 프로세스 메모리 덤프 + strings는 flaky하므로 릴리스당 1회 수동(§13.3).
  4. **회전 후 제거 멤버 복호 불가** — 제거 전 캡처한 구 GK로 회전 후 트래픽 복호 실패 + 재접속 거부 + revoke grace 0 확인. **위조 Revoked**(체인에 Remove(self) 부재) 수신 시 키 미파기·crypto-erase 미실행·무시 확인(강제 이탈·데이터 손실 차단, Critical).
  5. **AAD/재정렬 방어** — 서버 위치에서 SignalHdr·origin 변조 시 전 수신자 폐기, 구 signal replay 시 LWW 폐기.
  6. **로그 롤백 거부 + split-view 탐지** — 서버가 낡은 로그 tail 제시 시 클라 거부(head 단조성). presence head-hash 발산 시 동기화 차단·경고, **stale enc_profile 재생 시 단조 seq로 거부**(revoke 은닉 탐지, Major).
  7. 유지: 전역 IPv4/IPv6 목적지 거부, 로그 파일 내 클립보드 평문·**초대 코드** 미포함(서버 로그 포함), concealed 미전파, WebView 외부 fetch 차단(CSP), **외부 호스트 접속 0건 소켓 후킹 — 서버 프로세스에도 동일 적용**.
- 공급망: cargo audit + cargo deny CI 게이트 — 유지(spake2 제거로 감시 대상에서 제외, p256·x25519-dalek 추가 감시).

### 13.3 3플랫폼 수동 체크리스트 (릴리스 게이트)

서버(systemd·Docker 각 1회 설치) + macOS / Win11 / Linux X11 / Wayland-KDE / Wayland-GNOME(≥49·≤48) 클라 열에 대해: 서버 문서만으로 설치·기동(`fingerprint`·setup 토큰 절차 포함), 클라 설치→트레이 표시, **워크스페이스 생성(최초 기기, 지문 수동 대조)**, **초대 코드 조인 성공/오타 실패/재사용 거부/발급 기기 오프라인 상태에서 타 멤버 경유 완료**, 텍스트 양방향(한글/emoji), PNG 양방향 + Windows DIB 왕복, 연속 복사 루프 없음, sync off/on, **서버 재시작 → 전 클라 자동 복구**, Wi-Fi 전환(IP 변경) 후 재접속, 초대형 이미지 RSS<80MB 유지·스킵, **비멤버 기기 접속 거부**, **멤버 제거→제거 기기 신규 콘텐츠 미도달**, 유휴 CPU <1%·RSS <80MB(클라)·**서버 RSS/CPU 실측 기록**, **`sb-server verify-log`/`status` 무결성·claimed·head 점검, wslog torn-write(강제 킬 후 기동 트렁케이트 복구), `reclaim`으로 orphaned 워크스페이스 재클레임, 백업/이전 플레이북(지문 유지·신규 cert 재확인) 각 1회**, 절전 복귀, autostart 재부팅, **Wireshark: 클라↔서버 TLS·평문 미노출 + 서버 메모리 덤프 strings(릴리스당 1회)**. *(삭제: mDNS 발견, 페어링 코드 상호 대조)*

### 13.4 개발 환경

- **서버**: 맥 호스트 `cargo run` 로컬 실행이 기본 개발 루프 + Docker 이미지(linux/amd64·arm64 buildx)로 배포형 검증. 통합 테스트(1 서버 + 3 클라 loopback)는 Linux CI job 상시 실행.
- **VM**: Ubuntu GNOME(≤48)/Fedora(≥49)/Ubuntu X11/Kubuntu(KDE)/Win11 ARM — 유지. **bridged 필수 조건 해제**: mDNS가 없으므로 NAT VM에서도 호스트의 서버 주소 도달만으로 충분 — VM 구성 부담 감소.
- **CI**: 기존 3-OS 매트릭스 유지 + **서버 job**(ubuntu-latest + `container: ubuntu:22.04` 빌드, musl static, Docker 이미지 빌드, 통합·보안 회귀 스위트 실행). rust-cache + pnpm cache 유지. deb depends 검증 유지.
- **배포 문서 항목**: 방화벽 = **"서버 호스트 인바운드 TCP 1포트(45871)"로 단순화**(클라 인바운드·UDP 5353·멀티캐스트/reflector 협의 전부 삭제), 서버 호스트 최소 사양, server.toml 레퍼런스, systemd unit/compose 예시, 서버 이전·백업 절차(**그룹 키는 서버에 존재하지 않음** 명시, identity.key 취급 주의), 서버 먼저 업데이트 규칙(N5), 주기 회전 권장(§4.1 FS), GNOME 48 이하 XWayland 필수, data_dir 백업 제외 가이드(유지).
