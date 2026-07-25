# shareboard 구현 계획서 (v1.1 — 최종)

수석 아키텍트 통합안 — 9개 축 리서치 통합 + 3방향 비평(23건) 반영판. 리서치 간 모순은 §2에서 근거와 함께 단일 결정으로 확정함.

## 검증 과정에서 보강된 사항

- **보안**: DB dedup 해시를 keyed BLAKE3로 전환(unkeyed 해시 디스크 영속 금지), 페어링 세션 단일화·시도 카운터 원자 소진, IPv4/IPv6 명시 allowlist(전역 IPv6 차단), trust store 전용 MAC 키 상시 provisioning, concealed 검사·read의 동일 클립보드 세션 원자화 + 기본 제외 앱 동봉, CSP를 Tauri IPC 호환(`connect-src ipc: http://ipc.localhost`)으로 정정.
- **플랫폼**: GNOME Wayland 전략을 버전 기준으로 재작성(GNOME ≥49 = ext-data-control 1차 경로, ≤48 = XWayland 폴백), Windows DIB↔PNG 포맷 매트릭스와 다중 포맷 우선순위 규칙, 로컬 read 절대 상한(32MiB), 자기 IP 변경 시 리스너 재바인딩·mDNS 재등록 절차 신설.
- **운영**: 로깅 설계(tracing, 민감정보 로깅 금지, 회전/보존), 진단 패널(피어별 실패 사유·연결 테스트·로그 번들 내보내기), 수동 업데이트 배포와 혼재 버전 운영 가이드를 M6 산출물로 추가.
- **일정/CI**: M1을 3주로 확장하고 Wayland 직접 구현을 스파이크로 분리(wl-clipboard-rs MIT/Apache-2.0 코드 개작 지름길 명시), CI를 ubuntu-latest + `container: ubuntu:22.04`로 전환(22.04 러너 deprecation 대응), 합계 11.5주 + 25% 버퍼를 커밋 일정으로 명시.
- **minor 반영**: rusqlite 0.38/rusqlite_migration 2 페어 갱신, 썸네일 WebP→PNG(image 0.25 WebP 무손실 전용), spake2 미감사 사실 반영한 D3 근거 정정, Linux 키 폴백 사다리(keyutils→패스프레이즈 암호화→명시 동의 평문), 비식별 기본 장치명, 고위험 콘텐츠 확인 적용, wake/네트워크 변경 감지 수단 확정(D24), 3노드 통합 테스트 확장.

---

## 1. 개요와 목표

**shareboard**는 사내망(LAN) 전용 크로스 플랫폼(Linux/macOS/Windows) 클립보드 동기화 데스크톱 앱이다. 시스템 트레이 상주형이며, 승인(페어링)된 피어 간에만 UTF-8 텍스트와 PNG 이미지를 종단간 암호화로 동기화한다.

**목표**
- 클립보드 변경을 OS 이벤트 기반으로 감지하고, 변경 시에만 피어에게 신호(signal)를 보내 필요한 피어만 콘텐츠를 가져가는(fetch) 저대역폭 동기화.
- 페어링된 피어 외 모든 연결을 암호 핸드셰이크 단계에서 거부. 외부 서버·릴레이·텔레메트리 0. 사설망 주소(IPv4 사설 대역 + IPv6 ULA/링크로컬, §4.5 allowlist)에만 바인딩·통신.
- 유휴 CPU ~0%, 상주 RSS < 80MB 목표. 창 미표시 시 WebView 미상주.
- 피어 관리 / 클립보드 히스토리 / 동기화 on-off / 설정 UI + 진단 패널 + 앱·트레이 아이콘 제작.

**비목표 (명시적 범위 밖)**
- 모바일 지원, 파일 전송, 클립보드 "클리어" 동기화, 히스토리 전문 검색(FTS), 릴레이/NAT traversal/인터넷 경유 동기화(iroh류 금지), 자동 업데이트 **서버**(수동 업데이트의 배포 절차·문서는 M6 범위, §11), 오프라인 기간의 중간 히스토리 재동기화(최신 1건만 수렴), 동일 호스트 내 악성 프로세스의 OS 클립보드 직접 접근 방어(OS 권한 모델의 영역).

---

## 2. 아키텍처 결정 요약표

| # | 결정 | 근거 (한 줄) |
|---|---|---|
| D1 | Rust + Tauri 2 (2.11.x), 단일 프로세스 + tokio task 상주, 데몬 분리 없음 | uniclipboard의 19-crate/데몬 분리는 이 스코프에 과잉; Tauri 백엔드가 곧 상주 데몬 |
| D2 | 전송: **TCP + rustls 0.23 mutual TLS(1.3 전용) + 인증서 지문 pinning** | 감사된 TLS 스택(rustls) > 미감사 snow; Syncthing 검증 모델; 사내망 TCP가 UDP(QUIC)보다 방화벽 호환 우수 |
| D3 | 페어링: **SPAKE2(PAKE) + 6자리 코드, TLS 채널 바인딩** — Noise/snow 미채택, SAS 육안 대조 단독 미채택 | PAKE는 코드가 키 합의에 암호학적으로 결합. **spake2 크레이트는 미감사임을 인지하고 채택**: 노출을 60초 페어링 창의 1회성 교환으로 국한하고 TLS 채널 바인딩으로 이중화(전송 보안은 감사된 rustls 전담) — cargo audit/deny 감시 대상 및 §13.1 변조 테스트에 명시 포함 |
| D4 | 신원: 자기서명 인증서(ECDSA P-256, rcgen) 1개 = 장치 신원, device_id = SHA-256(SPKI) | 단일 키 모델로 단순화(Syncthing 동형); v1 키 회전 = 재페어링(N≤10 사내망에서 수용) |
| D5 | 직렬화: **serde + ciborium(CBOR)** — postcard 미채택, bincode 사용 금지 | `#[serde(default)]`로 버전 증가 없는 마이너 확장 가능(사내 순차 업데이트 현실 반영); LAN에서 크기 차 무의미; bincode는 생태계 사망 확정 |
| D6 | 탐색: **mdns-sd 0.18** + 수동 IP 추가를 1급 기능으로 병행 | 순수 Rust·외부 데몬 불요·양방향 유일; 사내망 멀티캐스트 차단이 흔해 수동 폴백 필수 |
| D7 | 클립보드: **clipboard-rs 0.3**(감시+I/O, Win/X11/macOS) + **wayland-client 직접 구현**(Wayland 이벤트 감시 — **wl-clipboard-rs(MIT/Apache-2.0)의 data-control 세션 코드를 감시 루프로 개작**, 제로부터 작성 금지) + **arboard 3.6** feature-flag 폴백 | clipboard-rs가 Win/X11 이벤트를 정확히 구현(소스 검증 완료); 단 Wayland watcher는 500ms 폴링임이 소스로 확인되어 직접 구현 필요; GPL인 wayland-clipboard-listener 회피 |
| D8 | macOS 감지: `NSPasteboard.changeCount` 정수 비교 워칭(기본 500ms, timer tolerance) | OS에 변경 이벤트 API 부재가 확정 사실 — "폴링 금지"를 "**콘텐츠 폴링 금지, 정수 카운터 비교 허용**"으로 요구사항 재정의 (본 문서로 확정) |
| D9 | 프로토콜: full-mesh, 릴레이 금지, signal→fetch 하이브리드(≤32KiB 텍스트는 inline push) | 소규모(N≤10)에서 릴레이는 불필요하며 루프 방지를 구조적으로 단순화; 소형 텍스트 1-RTT 즉시 반영 |
| D10 | 순서/충돌: Lamport clock 1차 + wall-clock 2차 + device_id 3차의 결정적 LWW | 시계 오차 무관한 인과 보존 + 전 노드 동일 승자 수렴 |
| D11 | 저장: 설정/피어 = JSON(atomic write), 히스토리 = rusqlite(bundled) + **앱레벨 XChaCha20-Poly1305 필드 암호화**, SQLCipher/sled 미채택 | 3-OS 빌드 부담 없는 순수 Rust 암호화; crypto-erase 용이; sled는 beta·메모리 과다 |
| D12 | 키 보관: keyring v3(OS keychain), Linux Secret Service 부재 시 폴백 사다리(§4.2) | 디스크 평문 키 저장 금지 원칙; 평문 폴백은 사다리 최하단·명시 동의 시에만 |
| D13 | 히스토리 디스크 영속화 기본 **off**(opt-in), 인메모리 30개는 기본 on | 보안 최우선 원칙 — 켜는 순간에도 비암호화 저장 경로 자체가 없음 |
| D14 | 프론트엔드: Svelte 5(runes) + Vite + TypeScript, 창은 close 시 destroy·재오픈 시 재생성 | 화면 4개 규모에 최소 런타임(~5KB); UI 상태는 Rust가 소유하므로 재생성 비용 낮음, WebView 메모리 상주 회피 |
| D15 | 이미지 전송: PNG 그대로(패스스루 우선, 재인코딩 시 Compression::Fast), zstd는 4KB 초과 텍스트에만 | PNG는 저장·해시·미리보기의 정규 형식; PNG 위 zstd 이중 압축은 무의미 |
| D16 | Windows 쓰기 시 `ExcludeClipboardContentFromMonitorProcessing` 등 제외 포맷 동시 기록 | 피어 콘텐츠가 Win+V 히스토리·클라우드 클립보드로 새는 것 차단(유출 차단 요건) |
| D17 | concealed 콘텐츠(비밀번호 관리자 힌트)는 기본 동기화 제외 + 히스토리 미저장. 검사와 read는 **동일 클립보드 세션에서 원자 수행**(§4.6). 힌트 미설정 매니저 대비 **기본 excluded_apps 목록 동봉** | nspasteboard.org 관례·Windows 제외 포맷·KDE 힌트 존중 + 힌트 지연 채움/미지원 환경의 유출 경쟁 차단 |
| D18 | 아이콘: C안 "Share Nodes"(클립보드+공유 노드), 트레이는 전용 글리프 + macOS template image | 32px 실측 렌더에서 식별성 최고; sync 모티프는 기존 도구와 겹침 |
| D19 | 기본 리슨 포트 고정 **TCP 45871** (설정으로 변경/0=random 가능) | 수동 IP 폴백과 사내 방화벽 규칙 협의에 결정적 포트 필요 (mDNS 실패 환경 대비) |
| D20 | 라이선스 방침: uniclipboard(AGPL) 코드 복사 금지, GPL 크레이트 회피. MIT/Apache-2.0 이중 라이선스 코드(wl-clipboard-rs 등)의 개작은 허용 | 아키텍처 참고만; 사내 배포도 전염 리스크 회피 |
| D21 | **DB 저장용 dedup/suppress 해시 = keyed BLAKE3**(키는 keychain, 히스토리 키와 동일 crypto-erase 수명). unkeyed blake3(ContentId)는 와이어·메모리 전용, **디스크 평문 영속 금지** | unkeyed 고속 해시의 디스크 저장은 저엔트로피 원문(비밀번호·PIN 등) GPU 사전 대입 역산을 허용 — 필드 암호화를 우회하는 구멍이므로 차단 |
| D22 | **trust store 무결성 = 전용 MAC 키**(keychain, 히스토리 설정과 무관하게 상시 provisioning)의 HMAC-SHA256 | `device_id==SHA-256(SPKI)` 검사는 위조 가능한 일관성 검사일 뿐 — 파일 쓰기 권한만으로 신뢰 피어 주입이 가능해지는 벡터를 독립 키로 차단 |
| D23 | 로깅: **tracing + tracing-subscriber/appender**(파일 회전), 민감정보(콘텐츠 평문·페어링 코드·키 재료) 로깅 **절대 금지**, panic hook 로컬 기록 | 보안 앱의 로그가 새로운 유출 경로가 되지 않도록 설계 단계에서 확정 |
| D24 | wake/네트워크 변경 감지: macOS `NSWorkspace` 슬립/웨이크 알림(objc2-app-kit) / Windows `WM_POWERBROADCAST` + `NotifyIpInterfaceChange`(windows-sys) / Linux logind `PrepareForSleep`(zbus) + rtnetlink. 폴백 = 60s 저주기 인터페이스 재열거 | if-addrs는 일회성 열거만 가능 — R7(절전 복귀)·자기 IP 변경(§4.5) 대응의 구현 수단을 사전 확정 |

---

## 3. 시스템 아키텍처

### 3.1 프로세스/모듈 구성

```
┌─ Tauri main process ─────────────────────────────────────────────┐
│  setup()에서 tauri::async_runtime(tokio 공유)으로 spawn:          │
│   ├─ clipboard watcher task   (OS 이벤트 → mpsc)                 │
│   ├─ mdns task                (register + browse, mdns-sd)        │
│   ├─ net listener task        (TCP accept → TLS → 피어 세션)      │
│   ├─ peer session tasks       (피어당 1개, FSM §5.4)              │
│   ├─ sync engine task         (signal→fetch 오케스트레이션)       │
│   ├─ power/net watch task     (D24: wake·인터페이스 변경 → 재바인딩)│
│   └─ store task               (히스토리/설정 영속화)              │
│  상태 공유: tauri::State<AppCore> — watch(스냅샷) + mpsc(작업)    │
├─ WebView (창이 열릴 때만 생성, close 시 destroy) ────────────────┤
│  Svelte 5 SPA — invoke(command) / listen(event)만 사용            │
└──────────────────────────────────────────────────────────────────┘
```

- 코어 로직은 `AppHandle` 비의존 순수 crate(sb-*)로 격리, Tauri 레이어(`src-tauri/src/`)는 얇은 어댑터. 코어는 채널로만 외부와 통신 → headless 통합 테스트 가능.
- 런타임은 Tauri 내장 tokio 하나만 사용(이중화 금지 — 메모리 절약).

### 3.2 데이터 흐름 (송신 경로)

```
OS 클립보드 이벤트
 → 150ms debounce(연타 코얼레싱)
 → [사전 판별] 포맷 목록·크기 조회(콘텐츠 read 전) — READ_HARD_LIMIT 32MiB 초과 시
   read 자체를 중단·항목 스킵 + UI 안내 (RSS<80MB 목표 보호)
 → suppress set 매칭? → yes: 소비(에코 차단)
 → [동일 클립보드 세션] concealed/제외 포맷 마커 확정 → 콘텐츠 read
   (마커 존재 시 read 내용 즉시 폐기·zeroize, §4.6)
 → blake3 해시 = ContentId → lamport += 1
 → 인메모리 히스토리(이미지는 항목당 5MB 초과 시 썸네일 강등) + 송신 캐시(5개/5분)
   [+opt-in 시 암호화 디스크 저장, dedup 키는 keyed BLAKE3 (D21)]
 → size > 10MiB? → 로컬 히스토리만, 전파 안 함
 → ClipSignal 생성 (≤32KiB 텍스트는 inline 동봉)
 → Ready 상태 전 피어 세션에 팬아웃 (TLS 채널)
```

### 3.3 데이터 흐름 (수신 경로)

```
ClipSignal 수신
 → LWW key 비교(§5.2) / suppress·LRU 검사 → 패배·중복이면 폐기
 → inline 있으면 즉시 적용; 없으면 ContentRequest → 청크 수신 → blake3 검증
 → [고위험 패턴 검사] 암호화폐 주소 형식·셸 명령형 문자열 등이면 자동 적용 대신
   알림 → 클릭 시 적용 (confirm_risky_content, 기본 on — 침해 피어의 클립보드
   하이재킹 완화, §4.1)
 → suppress set 등록 → OS 클립보드 set (Windows: 제외 포맷 동시 기록)
 → current_applied 갱신, lamport merge, 히스토리 기록, UI 이벤트 emit
```

### 3.4 IPC 경계

- UI→Rust command: `get_peers`, `start_pairing`, `submit_pairing_code`, `approve_peer` / `reject_peer` / `unpair_peer`, `add_manual_peer(addr)`, `get_history`, `copy_history_item(id)`, `delete_history_item(id)` / `clear_history`, `set_sync_enabled(bool)`, `get_settings` / `update_settings`, `pause_incognito(minutes)`, **진단**: `get_app_info`(앱/프로토콜 버전), `run_connection_test(addr)`, `export_diagnostics`(로그 번들)
- Rust→UI event: `peer-discovered`, `peer-online/offline`, `peer-conn-failed`(사유 코드: FingerprintRejected/Timeout/VersionMismatch/Refused), `pairing-started`, `pairing-code`, `pairing-result`, `clipboard-synced`, `clipboard-pending-confirm`(고위험 패턴), `sync-state-changed`, `history-updated`, `error`
- 대용량 payload는 emit 금지 — 이미지 썸네일은 command 응답으로 `tauri::ipc::Response`(바이너리). 히스토리 UI에는 절단 미리보기(앞 200자)/썸네일만 전달, 평문 전문을 WebView에 넘기지 않음(§4.6).

---

## 4. 보안 설계

### 4.1 위협 모델 요약

| 위협 | 대응 | 잔여 위험 |
|---|---|---|
| 수동 도청 (LAN sniffing) | 전 구간 TLS 1.3 (signal 메시지 포함, 평문 프레임 0) | 크기/타이밍 메타데이터 (v1 수용) |
| MITM / 피어 스푸핑 | mTLS + trust store 지문 pinning — 미지 지문은 핸드셰이크 단계 거부 | 없음 (페어링 정직 수행 전제) |
| mDNS 스푸핑/정찰 | mDNS는 주소 힌트 전용, 신뢰 판정 근거로 절대 미사용. 기본 device_name은 비식별 값(플랫폼 일반명+랜덤 접미사, 실명은 명시 설정 시에만) | 발견 방해 DoS → 수동 IP 폴백. reflector 환경의 세그먼트 외 광고 전파(배포 문서에 명시) |
| 페어링 코드 무차별 대입 | SPAKE2: **세션 단일화 + 원자 시도 카운터로 실행당 온라인 추측 정확히 1회**(§4.3), 오프라인 대입 불가. 60초 창·실패 즉시 코드 폐기·지수 백오프 | 10^-6 × 제한된 시도 기회. 공격자의 창 소진 DoS → 새 코드로 재개시 + 시도 IP UI 표시 |
| 디스크 키/히스토리 탈취 | 키는 OS keychain, 히스토리는 필드 암호화 + crypto-erase | OS 계정 자체 침해 (수용) |
| **히스토리 DB 해시 역산(사전 대입)** | dedup 해시 = keyed BLAKE3(D21), unkeyed 해시는 디스크 미영속 | keyed 키까지 탈취되는 OS 계정 침해 (수용) |
| **trust store(peers.json) 변조 — 공격자 키 주입** | 전용 MAC 키(D22) HMAC 검증, 실패 시 저장소 격리 + 전 피어 재페어링 유도 | 파일과 keychain 동시 침해 (수용) |
| 비밀번호 매니저 유출 | concealed 힌트 감지(동일 세션 원자 검사) → 기본 동기화 제외+미저장 (§4.6) | 힌트 없는 매니저 → **기본 excluded_apps 동봉** + 정규식 제외 규칙 + 최초 실행 시 위험 고지 |
| 침해된 승인 피어 | 즉시 revoke(세션 드롭+trust 삭제), 피어별 토글, last-seen 표시. **클립보드 하이재킹(정렬키 조작에 의한 임의 콘텐츠 강제 주입)**: 고위험 패턴 수신 시 확인 후 적용(§3.3) | revoke 이전 유출, 패턴 미검출 콘텐츠의 자동 적용 (수용) |
| Replay | TLS 1.3 세션별 ephemeral + 레코드 보호; fetch는 현재 세션 내만 유효 | 없음 |
| DoS | 미지 지문 즉시 드롭, IP별 token bucket, 프레임 256KiB 상한, 피어당 fetch 동시 1 | LAN 플러딩 (수용) |
| 다운그레이드 | TLS 1.3 전용, 협상 최소화, 앱 프로토콜 버전 불일치 = 거부 | 없음 |
| WebView 공격면 | CSP `connect-src ipc: http://ipc.localhost`(로컬 IPC 오리진만), capability 최소, innerHTML 금지, 원격 콘텐츠 0 | — |
| 외부 유출 | 아웃바운드 목적지를 주소 allowlist(§4.5, **IPv6 포함**)로 코드 레벨 제한, 텔레메트리/업데이트 체크 0건 | — |
| 로그 경유 유출 | 민감정보 로깅 금지 규칙(§4.7) + §13.2 로그 평문 검증 | — |
| 공급망 | cargo audit + cargo deny CI 게이트, Cargo.lock 커밋 (spake2 포함 감시) | 제로데이 (수용) |

### 4.2 신원과 키

| 키 | 알고리즘 | 수명 | 저장 |
|---|---|---|---|
| 장치 identity = 자기서명 TLS 인증서 keypair | ECDSA P-256 (rcgen 0.14, 유효기간 100년) | 장치 수명 | OS keychain (keyring v3) |
| device_id / 지문 | SHA-256(SPKI DER), 32바이트 | — | 공개 (peers.json, mDNS TXT) |
| 히스토리 암호화 키 | 256-bit 랜덤 | 장기, crypto-erase 시 파기 | OS keychain (identity와 분리 — 파기 정책 상이) |
| **dedup 해시 키 (D21)** | 256-bit 랜덤 (keyed BLAKE3용) | 히스토리 키와 동일 수명, crypto-erase 시 함께 파기 | OS keychain |
| **trust-store MAC 키 (D22)** | 256-bit 랜덤 (HMAC-SHA256) | 장치 수명, **히스토리 설정과 무관하게 첫 실행 시 상시 생성** | OS keychain |
| 세션 키 | TLS 1.3 파생 | 세션 | 메모리(rustls 내부) |
| 페어링 코드 / SPAKE2 산출 K | — | 60초 | 메모리, zeroize |

- v1 키 회전 정책: identity 회전 = 재페어링 (분리 키 + 회전 프로토콜은 v2 후보 — 사내 N≤10에서 재페어링 비용이 낮아 단순화 우선).
- **Linux Secret Service 부재 시 폴백 사다리** (identity·MAC 키 공통):
  1. kernel keyutils / gnome-keyring 등 대안 보안 저장 재시도
  2. 사용자 패스프레이즈에서 argon2로 파생한 키로 **암호화 파일** 저장(0600/0700)
  3. 명시 동의 시에만 평문 파일(0600, 시작 시 권한 검사) — 사용 중에는 설정 화면과 트레이에 **상시 경고** 표기, "전 피어 즉시 revoke" 원클릭 UX 제공
  - 어느 것도 불가하고 동의도 없으면: trust store **영속화 비활성**(세션 한정 페어링), 히스토리 키는 폴백 없이 영속화 비활성.
- 배포 문서에 data_dir이 백업/홈 동기화 폴더에 포함되지 않도록 경로 가이드 명시(사칭 키 유출 방지).

### 4.3 페어링 프로토콜 (SPAKE2 + TLS 채널 바인딩)

단일 포트(45871), 단일 TLS 스택 위에서 수행. 페어링 수락 모드는 사용자가 UI에서 명시적으로 열 때만 활성(60초 창).

```
0. 전제: 양측이 첫 실행 시 자기서명 cert 생성·keychain 보관.
1. [발견]  B가 mDNS 또는 수동 IP로 A를 발견. 신뢰 0.
2. [개시]  A 사용자가 "페어링 수락" 시작 → A가 CSPRNG 6자리 코드 생성·표시.
           60초 창. 전역 시도 카운터 = 1 (원자적, step 5 참조).
3. [연결]  B→A TLS 연결. 페어링 창 동안만 미지 인증서 수락(양측 cert는 이 시점 미신뢰).
           ** 진행 중 페어링 세션은 정확히 1개로 직렬화 — 두 번째 이후의 미지-지문
           동시 연결은 SPAKE2 개시 전에 즉시 거부(병렬 온라인 추측 원천 차단). **
4. [입력]  B 사용자가 코드 입력 → TLS 채널 안에서 SPAKE2 실행
           (password = 코드, identity = "pair-a"/"pair-b" 역할 태그 — 반사 공격 방지).
5. [확인]  transcript = H(버전 || SPAKE2 메시지 || A_cert지문 || B_cert지문)  ← TLS 채널 바인딩
           B→A: HMAC(K, "confirm-b" || transcript), A→B: HMAC(K, "confirm-a" || transcript)
           ** confirm 검증 '이전'에 전역 시도 카운터를 compare-and-set으로 원자 소진 —
           동시 도착한 복수 confirm이 같은 코드에 각각 평가되는 경쟁 상태 차단. **
           불일치 → 원자적으로 코드·K zeroize, 창 종료, 지수 백오프(2^n초),
           시도 발신 IP를 UI에 표시(공격자의 창 소진 DoS는 새 코드 재개시로 대응).
           → TLS를 MITM한 공격자는 transcript의 cert 지문 불일치로 반드시 실패.
6. [교환]  HKDF(K) 파생 AEAD로 {device_name, platform, proto range} 교환.
7. [저장]  양측 trust store에 {지문, cert, name, platform, paired_at} 등록 + MAC 재계산(D22).
           UI에 상대 전체 지문 표시(감사용 — KDE Connect의 8자 절단 취약점 교훈: 절단 금지).
8. [파기]  코드·K·SPAKE2 상태 zeroize. 코드 재사용 절대 금지.
```

### 4.4 전송 암호화 (상시 세션)

- rustls 0.23 + tokio-rustls 0.26, **TLS 1.3 전용**, 양방향 인증서 요구(mTLS).
- custom `ServerCertVerifier`/`ClientCertVerifier`: 체인/유효기간/호스트명 검증을 모두 생략하고 **"peer cert의 SHA-256(SPKI)가 trust store에 존재하는가" 단일 바이트 비교**만 수행. 미등록 지문 → 핸드셰이크 실패(페어링 창이 열려 있지 않은 한).
- 핸드셰이크 타임아웃 3초, 실패 IP는 token bucket(분당 5회) 스로틀.
- 동일 피어 중복 연결(동시 다이얼): device_id 사전식으로 작은 쪽이 initiator인 연결만 유지 — 결정적 tie-break.
- TLS 위 애플리케이션 핸드셰이크에서 `Hello.device_id ≠ TLS 피어 cert 지문`이면 즉시 종료(계층 간 신원 일치 강제).

### 4.5 네트워크 격리

1. **주소 allowlist (전 지점 공통)** — bind, accept, 아웃바운드 다이얼, mDNS 해석 주소(A/AAAA), 수동 피어 입력 모두에 동일 적용:
   - 허용: IPv4 `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, loopback / IPv6 **`fc00::/7`(ULA), `fe80::/10`(링크로컬, zone id(scope) 검증 필수), `::1`**
   - 그 외 전부 거부 — **전역 IPv6 포함**(mdns-sd는 AAAA도 해석하므로 명시 차단). 위반 주소는 바이트를 읽기 전에 close/거부.
2. `0.0.0.0`/`::` 바인딩 금지 — `if-addrs`로 인터페이스 열거, allowlist 인터페이스에만 리슨. 다중 NIC 시 설정에서 선택, VPN/터널 인터페이스는 기본 제외(옵트인).
3. 아웃바운드 목적지는 trust store 피어의 allowlist 주소만 — 코드 레벨 거부.
4. mDNS TXT = 프로토콜 버전 + 지문(공개 정보)만. device_name은 TXT에 포함하지 않고 애플리케이션 핸드셰이크에서 교환. 텔레메트리·크래시 리포트·업데이트 체크 아웃바운드 0건 (통합 테스트에서 소켓 후킹으로 검증).
5. **자기 주소 변경 처리(D24 연동)**: 인터페이스/주소 변경 감지 → 리스너 소켓 재바인딩 → mDNS 레코드 갱신(unregister→register) → 낡은 주소의 기존 세션 정리 후 재연결. Wi-Fi AP 전환·DHCP 갱신·유선↔무선 전환 시나리오를 §13.3 체크리스트로 검증.

### 4.6 데이터 보호

- 히스토리: SQLite content 필드만 XChaCha20-Poly1305 암호화 (row별 24B 랜덤 nonce, AAD = row id||kind||created_at — 행 바꿔치기 방지). dedup/suppress용 DB 해시는 **keyed BLAKE3(D21)** — unkeyed ContentId는 어떤 형태로도 디스크에 영속하지 않음. WAL에도 암호문만 기록됨. `secure_delete=ON`. 전체 삭제 = DELETE + VACUUM + keychain의 히스토리 키·dedup 키 파기·재생성(crypto-erase).
- concealed 감지/재표기: macOS `org.nspasteboard.ConcealedType`/`TransientType`, Windows `ExcludeClipboardContentFromMonitorProcessing`·`CF_CLIPBOARD_VIEWER_IGNORE`·`CanIncludeInClipboardHistory=0`·`CanUploadToCloudClipboard=0`, KDE `x-kde-passwordManagerHint=secret`. **검사 원자성**: 포맷 목록을 먼저 확정한 뒤 **동일 클립보드 세션**(Windows: 동일 `OpenClipboard` 세션, 지연 준비 재시도 이후에도 동일 원칙)에서 마커 검사와 콘텐츠 read를 수행 — 마커가 하나라도 있으면 read한 내용 즉시 폐기·zeroize(늦게 채워지는 제외 포맷을 놓치는 경쟁 차단). 기본 = 동기화 제외 + 미저장. 완화 모드에서도 수신 측이 동일 포맷 재표기. 힌트 미설정 매니저 대비: 알려진 비밀번호 매니저 프로세스의 **기본 excluded_apps 목록 동봉**, 힌트 미지원 환경에서는 최초 실행 시 위험 고지 + 정책 선택.
- 메모리: `zeroize`/`secrecy`로 키·코드·전송 버퍼 wipe. WebView/OS 버퍼는 통제 불가 → 평문을 IPC로 넘기지 않는 아키텍처가 1차 방어.
- Tauri 2: capability는 `core:default` + event listen/emit + autostart 3종 + opener만. shell/fs/http 플러그인 **미설치**. CSP: `default-src 'self'; img-src 'self' data: blob:; connect-src ipc: http://ipc.localhost` — Tauri 2의 invoke()는 fetch 기반 IPC 채널을 사용하므로 `connect-src 'none'`은 IPC 자체를 차단함(공식 권장 CSP 준수). 외부 아웃바운드 0 목표는 로컬 IPC 오리진 2개만 허용하는 것으로 동일하게 달성되며, §13.2에서 "WebView 외부 호스트 fetch 차단"을 별도 검증. remote capability 선언 금지. devtools feature는 release에서 비활성.

### 4.7 로깅과 민감정보 (D23)

- tracing + tracing-subscriber + tracing-appender. 로그 경로 `data_dir/logs/`(디렉터리 0700, 파일 0600), 일 단위 회전, 기본 보존 7일, 기본 레벨 INFO(설정으로 변경).
- **금지 규칙**: 클립보드 콘텐츠 평문·미리보기, 페어링 코드, 키 재료(SPAKE2 상태·세션 키·keychain 키)는 어떤 레벨에서도 로깅 금지. 콘텐츠는 keyed 해시 앞 8바이트 + 크기 + kind로만 지칭.
- panic hook으로 크래시를 로컬 로그에 기록(외부 전송 없음). 진단 번들 내보내기(§8.1)는 로그 + 익명화된 피어 상태만 포함.
- §13.2 보안 회귀 스위트에 "로그 파일 내 클립보드 평문/코드 미포함" 검증 포함.

---

## 5. 동기화 프로토콜

### 5.1 전제와 프레이밍

- 피어당 TLS 연결 1개. 프레이밍: `tokio_util::codec::LengthDelimitedCodec` (u32 LE length prefix, `max_frame_length = 256 KiB`). 초과 프레임 = 프로토콜 위반, 즉시 종료.
- 직렬화: serde + ciborium(CBOR). 마이너 확장은 `#[serde(default)]` 신규 필드(구버전은 무시), `Message` variant 추가는 반드시 버전 증가. 버전 지원 창 = 직전 1개 (혼재 기간 운영 절차는 M6 배포 문서, §11).

### 5.2 메시지 정의 (Rust enum 스케치)

```rust
pub type DeviceId  = [u8; 32];   // SHA-256(cert SPKI) — TLS 계층 신원과 동일
pub type ContentId = [u8; 32];   // blake3(content) — 콘텐츠 주소 + 무결성 검증값 (와이어·메모리 전용, 디스크 영속 금지 — D21)

pub const PROTO_MIN: u16 = 1;
pub const PROTO_MAX: u16 = 1;

#[derive(Serialize, Deserialize)]
pub struct Envelope { pub v: u16, pub msg: Message }

#[derive(Serialize, Deserialize)]
pub enum Message {
    // 애플리케이션 핸드셰이크 (TLS 확립 직후)
    Hello(Hello),            // device_id, device_name, platform, proto_min/max, app_version
    HelloAck(HelloAck),      // + chosen_version, head: Option<ClipMeta>, sync_enabled

    // 동기화 코어
    ClipSignal(ClipMeta),
    ContentRequest { id: ContentId },
    ContentBegin   { id: ContentId, total_size: u64, chunk_count: u32, chunk_size: u32 },
    ContentChunk   { id: ContentId, index: u32, data: Vec<u8> },
    ContentReject  { id: ContentId, reason: RejectReason }, // TooLarge|KindDisabled|Gone|SyncDisabled
    ContentAbort   { id: ContentId, reason: AbortReason },  // Superseded|Cancelled|InternalError

    // 상태/유지
    Ping { nonce: u64 },  Pong { nonce: u64 },
    SyncState { enabled: bool },
    Bye { reason: ByeReason },       // Shutdown|Unpaired|ProtocolError
    Error { code: ErrorCode, detail: String },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClipMeta {
    pub id: ContentId,
    pub kind: ContentKind,           // Text | ImagePng
    pub size: u64,
    pub lamport: u64,                // LWW 1차 정렬키
    pub wall_ts_ms: u64,             // 2차 정렬키 + UI 표시
    pub origin: DeviceId,            // 릴레이 금지이므로 = 송신자 (UI/디버깅용)
    pub inline: Option<Vec<u8>>,     // size <= 32KiB 텍스트면 원문 동봉
}

// 페어링 전용 (페어링 창에서만 처리, 세션 1개로 직렬화 — §4.3)
#[derive(Serialize, Deserialize)]
pub enum PairingMsg {
    PakeMsg   { role: PairRole, payload: Vec<u8> },   // SPAKE2 메시지
    Confirm   { hmac: [u8; 32] },                      // transcript 결합 key confirmation
    Info      { name: String, platform: Platform, proto_min: u16, proto_max: u16 }, // AEAD 봉인
    Abort     { reason: PairAbortReason },
}
```

**기본 파라미터**: INLINE_THRESHOLD 32KiB · CHUNK_SIZE 64KiB · MAX_CONTENT_SIZE 10MiB(전파 상한, 설정 1–100MiB) · **READ_HARD_LIMIT 32MiB(로컬 read 절대 상한, §3.2)** · heartbeat idle 15s Ping(데이터 수신도 liveness 인정) · dead 판정 45s · 재연결 백오프 1s→×2→상한 60s(±20% jitter, wake/네트워크 변경/mDNS 재발견 시 리셋 — D24) · 로컬 debounce 150ms · 송신 캐시 5개/5분 TTL(만료 시 `Gone`) · signal rate limit 피어당 10건/s · fetch 타임아웃 10s(첫 청크·청크 간) · 텍스트 4KB 초과 시 zstd level 3.

### 5.3 순서 판정(LWW)과 루프 방지

```
key(item) = (lamport, wall_ts_ms, origin_device_id)   // 사전식 비교
key(incoming) > key(current_applied) 일 때만 로컬 클립보드 반영
```
- lamport: 로컬 복사 시 +1, 원격 수신 시 max-merge → 인과 순서는 벽시계 오차와 무관하게 보존. 동시 복사는 wall-clock으로 "나중에 복사한 쪽이 이김", 완전 동률은 device_id 결정적 tie-break → 전 노드 수렴. 패자 콘텐츠는 히스토리에 보존(UI 안내).
- 정렬키는 송신 피어가 채우므로 침해 피어의 조작(항상 승자화)이 가능 — 자동 적용의 고위험 패턴 확인(§3.3)으로 완화하고 §4.1 위협표에 명시.

**에코 루프 3중 방어**
1. **suppress set**: 원격 콘텐츠를 OS 클립보드에 쓰기 직전 ContentId 등록 → 이후 로컬 이벤트 해시가 매칭되면 브로드캐스트 없이 소비. Windows의 다중 이벤트 발화 대비 등록 후 2s 유예 창 동안 반복 매칭 허용.
2. **recent-hash LRU(16)**: 수신 신호가 현재 적용 항목과 같거나 LRU에 있고 key가 크지 않으면 무시 → A→B→A 핑퐁 차단. 의도적 재복사는 lamport 증가로 정상 전파.
3. **릴레이 금지**: 수신 신호를 절대 재브로드캐스트하지 않음(full-mesh 구조가 다단 루프를 원천 차단). 추가로 이미지 해시는 PNG 정규화 후 계산 — OS가 바이트를 변형하는 루프(uniclipboard #1422 교훈) 완화.

### 5.4 피어 연결 FSM

```
[Idle] → (mDNS 발견/백오프 만료/수동 연결) → [Connecting: TCP+mTLS]
  → 지문 검증 실패/타임아웃 3s → [Backoff] (실패 사유 코드를 UI 진단 패널에 기록)
  → [Handshaking: Hello/HelloAck, 버전 협상, 신원-지문 일치 검사, 10s 타임아웃]
  → [Ready] ←→ [Degraded: 45s 무수신 → Ping 재시도] → [Disconnected] → [Backoff] → 재시도
Ready 진입 직후: HelloAck.head 간 key 비교 → 상대가 최신이면 inline 적용 또는 fetch
  (절전/오프라인 수렴은 최신 1건만 — 전체 재동기화 없음)
로컬 주소 변경(D24) → 리스너 재바인딩·mDNS 재등록 → 전 세션 [Disconnected] → 즉시 재시도
```

수신측 콘텐츠 전송 FSM: Evaluate(key/크기/종류 검사) → Requesting → Receiving(index 순서 검증, 더 높은 key 신호 도착 시 `Abort(Superseded)`) → Verifying(blake3 일치, 불일치 시 1회 재요청) → Applying(suppress 등록 → set → lamport merge). 피어당 인바운드 전송 동시 1건.

### 5.5 엣지케이스 처리 방침 (요지)

- 동기화 off: 브로드캐스트 중단 + 수신 무시 + `SyncState{false}` 통지, 재개 시 head 재교환.
- 빈 클립보드/클리어: 전파하지 않음(v1 제외). 연타 복사: debounce로 마지막만.
- **다중 포맷 동시 존재(Excel/브라우저 복사 등)**: 판별 우선순위 = concealed 마커 존재 시 전체 제외 > 텍스트 > 이미지. v1은 단일 kind만 전파(텍스트가 의미 원본인 경우가 다수 + 붙여넣기 호환성·크기 최소화). 멀티 kind 동봉은 v2 후보.
- **텍스트 정규화**: UTF-8 원문 바이트 그대로 전송 — CRLF/LF 개행 변환 금지(변환은 해시 불일치·에코 오판 유발). X11 read는 UTF8_STRING 우선, STRING(Latin-1)은 UTF-8 변환, COMPOUND_TEXT 미지원(스킵). write는 플랫폼 관례 포맷 동시 기록(§6 매트릭스).
- set 실패(Wayland 포커스 제약 등): 히스토리 기록 + UI 수동 "클립보드로 복사" 제공.
- X11 lazy clipboard(Chrome 등): 소유권 이벤트 직후 짧은 지연+재시도, INCR 처리. Windows `OpenClipboard` 경합: 100ms×10회 지수 백오프, `GetClipboardSequenceNumber`로 중복 알림 디듀프.
- wall_ts가 5분 이상 미래면 로그 경고(클램프 금지 — 노드 간 판정 분기 방지).
- inline 콘텐츠 해시 불일치 = 프로토콜 위반: 항목 폐기+로그, 반복 시 연결 종료.
- 버전 협상 실패: `Error(VersionIncompatible)` + UI "상대 앱 업데이트 필요"(진단 패널에 양측 버전 표시).

---

## 6. 플랫폼별 클립보드 전략

| 플랫폼 | 감지 | 방식 | 비고 |
|---|---|---|---|
| Windows | 완전 이벤트 | 메시지 전용 히든 윈도우 + `AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE` (clipboard-rs watcher) | Win11 알림 직후 데이터 미준비 → 50–100ms 지연 후 **동일 OpenClipboard 세션에서 포맷 목록+concealed 마커+데이터를 원자적으로 읽기**(§4.6). 쓰기 시 제외 포맷(D16) 동시 기록 |
| macOS | changeCount 워칭 | `NSPasteboard.general.changeCount` 정수 비교, 기본 500ms + timer tolerance(coalescing) | 순수 이벤트 API 부재(확정) — D8 요구사항 재정의. 화면 잠금/슬립 시 정지, 동기화 off 시 워처 완전 정지. TIFF→PNG 정규화(clipboard-rs 내장) |
| Linux X11 | 완전 이벤트 | XFixes `SelectionNotify` (clipboard-rs, x11rb) | 이벤트의 소유자 윈도우 == 자신이면 무시(루프 방지 보조). lazy target 재시도 |
| Linux Wayland (KWin/wlroots/**GNOME ≥49**) | 완전 이벤트 | `ext-data-control-v1` 우선, `zwlr_data_control_v1` 폴백 — **wayland-client 0.31 + wayland-protocols 직접 구현, wl-clipboard-rs(MIT/Apache-2.0)의 data-control 세션 코드를 감시 루프로 개작**(D7·D20), I/O는 wl-clipboard-rs 0.9 | Mutter는 GNOME 49(2025-09)부터 ext-data-control-v1 구현(KWin 6.6·Sway 1.11도 지원) — GNOME 49+는 1차 경로가 그대로 동작, 추가 작업 불필요. clipboard-rs Wayland watcher(500ms 폴링)는 미사용 |
| Linux Wayland (**GNOME ≤48** — Ubuntu 22.04=42/24.04=46, RHEL9=40) | 폴백 사다리 | ① data-control 바인딩 시도(런타임 감지) → ② 실패 시 XWayland(X11+XFixes) 경유 — Mutter가 Wayland↔X 클립보드 자동 동기화. **XWayland 부재 시 런타임 감지 + 설정 화면 안내** → ③ opt-in 저주기 폴링(기본 off) + 사유 표기 | LTS 배포판이 사내 주력일 가능성이 높은 실질 리스크 구간(R2). GNOME 49에서 X11 세션 컴파일 타임 비활성·50에서 제거 추세이므로 XWayland 폴백에 장기 의존 금지 — 배포 문서에 "GNOME 48 이하는 XWayland 필수" 명시 |

**포맷 매트릭스 (read 수용 → 정규형 / write 동시 기록)**

| 플랫폼 | read → 정규형(UTF-8 텍스트 / PNG) | write 동시 기록 |
|---|---|---|
| Windows | `CF_UNICODETEXT`→UTF-8(개행 원문 보존); 이미지: 등록 포맷 `"PNG"` 우선 → `CF_DIBV5` → `CF_DIB` → PNG 변환(**clipboard-rs 내장 여부 M1에서 검증, 미비 시 image 크레이트로 자체 변환**) | `CF_UNICODETEXT`; `"PNG"` + `CF_DIBV5`(+`CF_DIB`) — PNG만 기록하면 다수 앱 붙여넣기 불가; + 제외 포맷(D16) |
| macOS | `public.utf8-plain-text`; TIFF→PNG | `public.utf8-plain-text`; PNG(TIFF는 OS 자동 제공) |
| X11 | `UTF8_STRING` 우선, `STRING`(Latin-1)→UTF-8; `image/png` | `UTF8_STRING`+`STRING`; `image/png` |
| Wayland | `text/plain;charset=utf-8`; `image/png` | 동일 |

- 공통 추상화: `sb-clipboard`가 `ClipboardWatcher` / `ClipboardIo` trait 제공, 백엔드는 런타임 감지로 선택. 코어 로직은 mock 클립보드로 headless 테스트.
- 감시 이벤트 수신 시에도 signal→fetch 원칙 적용: changeCount/시퀀스 번호 + 포맷 목록·크기로 1차 판별 후에만 실제 데이터 read(대용량 이미지 이벤트 폭주 방지, READ_HARD_LIMIT 사전 차단 — §3.2).
- 소유권 유지(X11/Wayland): 쓰기 후 데이터 요청에 응답하는 스레드 상주(트레이 상주형이라 자연 충족).

---

## 7. 저장소/히스토리/설정

### 7.1 레이아웃 (파일 0600, 디렉터리 0700; Windows는 %LOCALAPPDATA% 기본 ACL)

```
config_dir/settings.json          # atomic write (tempfile persist → rename)
data_dir/identity.json            # 공개 부분만 (secret은 keychain — 폴백 사다리는 §4.2)
data_dir/peers.json               # trust store: {지문, cert, name, status, sync_enabled, paired_at, last_seen, last_addr} + mac 필드
data_dir/history.db (+wal/shm)    # rusqlite bundled, WAL, secure_delete=ON
data_dir/logs/                    # tracing 파일 로그, 일 단위 회전·7일 보존 (§4.7)
```
- Windows는 Roaming 금지(Local 사용) — identity가 로밍 프로파일로 복제되면 안 됨.
- **peers.json 무결성(D22)**: 캐노니컬 직렬화 위에 전용 trust-store MAC 키(keychain 상시 provisioning, 히스토리 설정과 무관)로 HMAC-SHA256 태그를 `mac` 필드에 기록. 로드 시 반드시 검증 — 불일치 시 저장소를 `.quarantine`으로 격리하고 빈 신뢰 상태로 기동 + "전 피어 재페어링 필요" 경고 표시. `device_id == SHA-256(cert SPKI)` 재검증은 일관성 검사로만 병행(공격자가 일관된 값을 넣으면 통과하므로 **무결성 근거로 사용하지 않음**). Linux 파일 폴백 모드에서는 MAC 키도 identity와 같은 사다리(§4.2)로 보관하고, 안전 보관 불가 시 trust store 영속화 자체를 비활성.

### 7.2 히스토리 스키마 (요지)

```sql
CREATE TABLE history (
  id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL,
  kind TEXT CHECK (kind IN ('text','image')), origin TEXT NOT NULL,  -- 'local' | peer_id
  dedup_mac BLOB NOT NULL UNIQUE,           -- keyed BLAKE3(dedup 키, content) — D21.
                                            -- unkeyed blake3 저장 금지(저엔트로피 원문 사전 대입 역산 차단)
  size_bytes INTEGER NOT NULL, pinned INTEGER DEFAULT 0,
  preview_nonce BLOB, preview_ct BLOB,      -- 암호화: 텍스트 앞 256자 / PNG 썸네일(긴변 256px)
  body_nonce BLOB, body_ct BLOB             -- 암호화 원본 (AAD = id||kind||created_at). NULL = 썸네일만(강등)
);
```
- 썸네일 포맷은 **PNG**(png 크레이트 기존 의존, 256px에서 용량 충분히 작음) — image 0.25의 WebP 인코딩은 무손실 전용이라 스크린샷류에서 수십~수백 KB로 커지는 문제로 미채택.
- suppress set 연동은 인메모리 ContentId(unkeyed)로, DB dedup(UNIQUE)은 keyed_hash로 — keyed 해시도 결정적이므로 dedup 동작 동일. dedup 키는 히스토리 키와 함께 crypto-erase(§4.2).
- 정책 기본값: 인메모리 30개(on) / 디스크 영속화 **off(opt-in)** / 최대 200개·7일 / 이미지 원본 항목당 ≤5MB·최근 20개만(초과분 썸네일 강등, 인메모리에도 동일 상한) / concealed 제외 on / pinned는 retention 제외.
- 검색은 복호화 후 인메모리(수백 항목 규모에서 수 ms) — 본문 SQL 검색 불필요.
- 정리: 앱 시작 + 1시간 주기 DELETE, 전체 삭제 시 crypto-erase(§4.6).
- 마이그레이션: `rusqlite_migration` + `PRAGMA user_version`, JSON은 `version` 필드 + 알 수 없는 키 보존(round-trip). **rusqlite와 rusqlite_migration은 항상 한 쌍으로 버전업**(호환 창 불일치로 인한 컴파일 충돌 방지 — 현행 페어: rusqlite 0.38 + rusqlite_migration 2.x).

### 7.3 settings.json 기본값

```json
{
  "version": 1,
  "sync":    { "enabled": true, "sync_text": true, "sync_images": true,
               "max_content_bytes": 10485760, "auto_apply_received": true,
               "confirm_risky_content": true },
  "history": { "memory_max_items": 30, "persist_enabled": false, "max_items": 200,
               "retention_days": 7, "store_image_originals": true,
               "max_image_item_bytes": 5242880, "max_image_originals": 20 },
  "privacy": { "exclude_concealed": true,
               "excluded_apps": ["<동봉: 알려진 비밀번호 매니저 프로세스 목록>"],
               "exclude_patterns": [] },
  "network": { "listen_port": 45871, "mdns_enabled": true, "interface": null,
               "manual_peers": [], "device_name_override": null },
  "app":     { "autostart": false, "start_minimized_to_tray": true,
               "notify_on_receive": true, "notify_on_peer_request": true,
               "log_level": "info", "language": "system", "theme": "system" }
}
```
- `device_name_override: null`일 때 기본 장치명은 **비식별 값**(플랫폼 일반명 + 랜덤 4자, 예: `mac-3f7a`) — 호스트명("CEO-Macbook" 등)의 mDNS reflector 경유 세그먼트 외 노출 방지(§4.1). 실명은 사용자가 명시 설정할 때만.

---

## 8. UI/UX

### 8.1 화면 구성 (창 1개, 좌측 탭)

1. **Peers**: 3분할 — 발견됨(mDNS, [페어링] 버튼) / 페어링됨-온라인 / 페어링됨-오프라인. 항목: 이름, 플랫폼 아이콘, 전체 지문(축약 표시 + 클릭 시 전체 — 절단 지문만으로 신뢰 판단 금지), 마지막 동기 시각, **마지막 연결 실패 사유 코드**(지문 거부/타임아웃/버전 불일치/거부 구분 — `peer-conn-failed`), 피어별 sync 토글, revoke. 하단 "수동으로 추가 (IP:port)" — mDNS 차단망 1급 폴백.
2. **페어링 다이얼로그(모달)**: 수락 측 — 6자리 코드 대형 표시 + 60초 카운트다운 + 실패 시도 발신 IP 표시. 요청 측 — 코드 입력 필드. 성공 시 양측 전체 지문 표시. 모든 전이(수락/거절/타임아웃/취소)를 상대에 전파(uniclipboard 페어링 버그 교훈).
3. **History**: 리스트(텍스트 절단 미리보기 / 이미지 썸네일, 출처 피어, 시각), 클릭 재복사, 개별/전체 삭제, pin, 고위험 패턴 수신 항목의 "확인 후 적용" 배지. 항목 상한 200으로 가상 스크롤 회피.
4. **Settings**: §7.3 항목 + 리슨 인터페이스 선택, keychain/폴백 모드 표기(평문 폴백 시 상시 경고), Wayland 제한 사유 표기, **앱 버전·프로토콜 버전 표기**, **진단 패널**: 리슨 상태(주소:포트), "연결 테스트(IP:port)" 버튼(TCP 도달→TLS→지문 판정 단계별 결과), 진단 로그 번들 내보내기(`export_diagnostics`) — R4 트러블슈팅에서 mDNS 차단/방화벽/지문 불일치/버전 비호환/인터페이스 오선택을 구분 가능하게 함. M6 사내 배포 문서의 IT 절차와 연동.

### 8.2 트레이

- 메뉴 중심 설계(Linux 좌클릭 제약 대응): 동기화 on/off, 창 열기, N분 일시정지(15/60), 히스토리 전체 삭제, 종료.
- 상태 표현: macOS는 template image(흑백+알파) — 정상/동기화 중(전송 중에만 2–3프레임 `set_icon` 교체, 상시 애니메이션 금지)/오프라인(알파 40%)/일시정지(∥ 배지). Windows/Linux는 색 배지 병용(#38BDF8 동기화, #F59E0B 일시정지, #94A3B8 오프라인).
- 창 close = destroy(WebView 메모리 회수), `RunEvent::ExitRequested`에서 `prevent_exit`. macOS `ActivationPolicy::Accessory`(Dock 숨김), Windows `skip_taskbar`.
- Linux: libayatana-appindicator 의존을 패키지 depends에 명시. GNOME 확장 부재로 트레이 미표시 가능 → 앱 재실행/CLI로 창 열기 폴백 제공.
- autostart: tauri-plugin-autostart (macOS LaunchAgent 방식, `--minimized` 인자).

---

## 9. 아이콘/브랜딩

- **채택: C안 "Share Nodes"** — 클립보드 + 공유 노드 글리프. 32px 실측 렌더 검증에서 식별성 최고(A안 원형 화살표는 소형에서 방향성 소실, B안은 OS "복사" 아이콘과 혼동 위험).
- 팔레트: primary `#2563EB`, glyph `#FFFFFF`, 상태색 syncing `#38BDF8` / connected `#22C55E` / paused `#F59E0B` / offline `#94A3B8` (모두 중간 명도로 라이트/다크 겸용).

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

- 트레이는 별도 전용 글리프(클립보드+⇄, 순수 검정+알파, 16/22px 검증 완료 — 리서치 산출 `tray-template.svg`) 사용. macOS `icon_as_template: true`.
- 파이프라인: `resvg`(Tauri CLI와 동일 렌더러)로 SVG→1024 PNG → `cargo tauri icon` → 트레이 상태별 PNG(22pt+@2x / 16·32px / 22·24px)를 `src-tauri/icons/tray/`에 별도 생성. 전 과정을 `scripts/gen-icons.sh`로 고정. 상태별 PNG는 `include_bytes!`로 내장, 런타임엔 `set_icon` 교체만.

---

## 10. 프로젝트 구조

```
shareboard/
├─ src/                            # 프론트 (Svelte 5 + Vite + TS)
│  ├─ main.ts / App.svelte
│  ├─ lib/
│  │  ├─ ipc.ts                    # invoke/listen 타입 안전 단일 진입점
│  │  ├─ stores/                   # peers.svelte.ts, history.svelte.ts, settings.svelte.ts
│  │  └─ components/               # PeerList, PairingDialog, HistoryList, SettingsPanel, DiagnosticsPanel
│  └─ styles/
├─ src-tauri/
│  ├─ Cargo.toml                   # workspace root
│  ├─ tauri.conf.json (+ tauri.conf.dev.json — dev CSP 완화 merge)
│  ├─ capabilities/main.json
│  ├─ icons/  (tauri icon 산출물)  /  icons/tray/  (상태별 트레이 PNG)
│  ├─ src/
│  │  ├─ main.rs                   # lib 호출만
│  │  ├─ lib.rs                    # Builder 조립, setup에서 core spawn
│  │  ├─ tray.rs / commands.rs / events.rs   # 얇은 어댑터 — Tauri 타입은 여기 밖으로 못 나감
│  └─ crates/                      # UI 독립 코어 (workspace members)
│     ├─ sb-core/                  # 도메인 타입, sync engine, LWW/suppress, 설정
│     ├─ sb-clipboard/             # Watcher/Io trait + win/x11/wayland/mac 백엔드, concealed 필터, 포맷 매트릭스
│     ├─ sb-net/                   # mdns-sd, TLS 리스너/다이얼러, 주소 allowlist, 피어 세션 FSM, 프레이밍
│     ├─ sb-crypto/                # identity cert, SPAKE2 페어링, custom verifier, trust store(MAC)
│     ├─ sb-store/                 # rusqlite 히스토리(필드 암호화, keyed dedup), JSON 영속화, keyring
│     └─ sb-platform/              # D24: wake/네트워크 변경 감지 플랫폼 바인딩
├─ assets/icons/                   # SVG 마스터 (app-icon.svg, tray-template.svg)
├─ scripts/gen-icons.sh
├─ .github/workflows/build.yml     # 3-OS 매트릭스 — Linux는 ubuntu-latest + container: ubuntu:22.04 (§13.4)
└─ package.json
```

의존 방향: `UI → (IPC) → src-tauri 어댑터 → sb-core → {sb-clipboard, sb-net(→sb-crypto), sb-store, sb-platform}`. sb-* 상호 직접 의존 금지(sb-net→sb-crypto 예외). 코어는 CLI로 단독 구동 가능(통합 테스트용).

**주요 크레이트 (버전 고정)**: tauri 2.11 / tauri-plugin-autostart 2.x / tokio 1.53 / tokio-util 0.7(codec) / mdns-sd 0.18 / rustls 0.23 / tokio-rustls 0.26 / rcgen 0.14 / spake2 0.4(미감사 — D3 노출 국한 전제, audit 감시 대상) / hkdf 0.12 / hmac 0.12 / sha2 0.10 / chacha20poly1305 0.10 / blake3 1.8(keyed 모드 포함) / argon2 0.5(파일 폴백 암호화) / zeroize 1.8 / secrecy 0.10 / keyring 3(플랫폼 feature 명시) / serde 1 / ciborium 0.2 / serde_json 1 / clipboard-rs 0.3 / arboard 3.6(feature flag) / wl-clipboard-rs 0.9 / wayland-client 0.31 + wayland-protocols / **rusqlite 0.38(bundled) / rusqlite_migration 2**(한 쌍 버전업 규칙 — §7.2) / png 0.18 / image 0.25(리사이즈·DIB↔PNG 변환) / zstd 0.13 / if-addrs 0.13 / **tracing 0.1 / tracing-subscriber 0.3 / tracing-appender 0.2**(D23) / **objc2-app-kit(macOS wake) / windows-sys(WM_POWERBROADCAST·NotifyIpInterfaceChange) / zbus 5(logind) + rtnetlink(Linux)**(D24) / dirs 6 / tempfile 3 / rand(OsRng). **bincode 금지**(생태계 사망), **iroh 금지**(외부 인프라 내장), **GPL 크레이트 금지**(MIT/Apache-2.0 이중 라이선스 개작은 허용 — D20).

---

## 11. 마일스톤

| # | 이름 | 범위 | 완료 기준 | 기간 |
|---|---|---|---|---|
| **M1** | 스켈레톤 + CI + 클립보드 감시 | workspace 구성(sb-* + Tauri), 최소 트레이(아이콘+Quit), **CI: ubuntu-latest+`container: ubuntu:22.04`** / macos / windows 매트릭스 그린, 로깅 기반(tracing, §4.7), `ClipboardWatcher` trait + Win/X11/macOS 백엔드, **Wayland 스파이크(1주 분리 배정): wl-clipboard-rs 코드 개작 data-control watcher + KWin/Sway/Mutter 49 실검증**, GNOME ≤48 폴백 사다리의 런타임 감지 골격, Windows DIB↔PNG 변환의 clipboard-rs 내장 여부 검증, **사내 표준 데스크톱/GNOME 버전 실사** | 3-OS CI 빌드 그린. 각 OS에서 텍스트 복사 시 즉시 (keyed) 해시 로그 출력, 유휴 CPU ~0% 실측. Wayland 지원 매트릭스(배포판×세션, **GNOME 49+ 열 포함**) 문서 확정 | 3주 |
| **M2** | 평문 텍스트 동기화 (수동 IP) | TCP + LengthDelimitedCodec + ciborium, Hello/Signal/Fetch 프로토콜, **에코 루프 3중 방어**, LWW key, **주소 allowlist(IPv4+IPv6, §4.5) 바인딩**. 프레이밍에 TLS 삽입 자리를 미리 설계(M3에서 교체만) | macOS + Linux VM(bridged) 2대에서 수동 IP 등록 후 양방향 텍스트 동기화, 연속 복사 루프 미발생, 전역 IPv6 목적지 거부 확인 | 1.5주 |
| **M3** | 암호화 + 페어링 + mDNS | rcgen identity cert + keyring 보관(폴백 사다리 포함), rustls mTLS + 지문 pinning verifier, SPAKE2 페어링(60초 창·**세션 단일화·원자 시도 카운터**·백오프), trust store + **전용 MAC 키 무결성(D22)**, mdns-sd 광고/탐색 | (1) 자동 발견 (2) 코드 페어링 성공·오입력 실패·**병렬 연결 거부** (3) 미페어 기기 핸드셰이크 거부 (4) Wireshark 캡처 평문 미노출 (5) peers.json 변조 시 격리 동작 (6) 보안 회귀 테스트 통과 | 2주 |
| **M4** | 이미지 + 히스토리 | PNG 감지/청크 전송(10MiB 상한, Supersede/Abort), **READ_HARD_LIMIT·사전 크기 판별(§3.2)**, Windows 포맷 매트릭스(DIB↔PNG) 구현, rusqlite 암호화 히스토리(opt-in 영속화, **keyed dedup(D21)**, crypto-erase), concealed 필터(**동일 세션 원자 검사**), 재복사 API, **GNOME ≤48 XWayland 폴백 실검증** | 맥 스크린샷 → Win/Linux 붙여넣기 + **Windows DIB 원본 → 타 OS → Windows 붙여넣기 왕복**. 히스토리에서 과거 항목 복원. 비밀번호 매니저 복사 항목 미전파. 초대형 이미지(100MB급) 복사 시 RSS 유지·스킵 동작 확인 | 1.5주 |
| **M5** | UI 완성 + 트레이 고도화 | Svelte 화면 4개 + **진단 패널(§8.1)**, 페어링 다이얼로그, 고위험 콘텐츠 확인 흐름, 트레이 상태 아이콘/메뉴, 앱 아이콘 파이프라인(gen-icons.sh), autostart. M4와 병행 가능 | 터미널 없이 UI만으로 페어링→동기화→히스토리→revoke 전 과정 시연, 진단 패널로 연결 실패 사유 구분 시연, 3-OS 트레이 동작 확인 | 2주 |
| **M6** | 패키징/배포 | dmg(zip 병행) / NSIS(+조직 정책 시 MSI) / deb·rpm(+검증 후 AppImage), 의존성(webkit2gtk-4.1, appindicator) 명시, 사내 배포 문서(방화벽 TCP 45871 + UDP 5353, mDNS/reflector 노출 주의, GNOME 48 이하 XWayland 필수, Gatekeeper/SmartScreen 안내, data_dir 백업 제외 가이드), 서명 절차, **업데이트 배포 문서: 배포 채널(사내 파일서버/MDM/GPO) 정의, 신규 버전 고지 수단, 프로토콜 혼재 기간 권장 절차(전 기기 v→v+1 순차 갱신, 지원 창=직전 1개 전제), settings/DB 마이그레이션 실패 대비 롤백 방침** | 클린 VM/실기기 3종에서 설치 파일만으로 설치→재부팅→자동 상주→페어링→동기화 성공. §13.3 체크리스트 전항목 통과 | 1.5주 |

**합계 11.5주(1인) + 20–30% 버퍼 = 커밋 일정 약 14주.** 버퍼 미포함 수치를 대외 커밋에 사용하지 않는다. M4/M5 병행 시 2인 기준 단축 가능. 코드사이닝 인증서(Apple Developer 1계정 / 사내 CA + GPO 신뢰 배포)는 리드타임이 길므로 **M3 시점에 착수**.

---

## 12. 리스크와 완화책

| # | 리스크 | 확률/영향 | 완화책 |
|---|---|---|---|
| R1 | macOS 이벤트 API 부재 | 확실/중 | D8로 요구사항 재정의(changeCount 정수 비교, 콘텐츠 폴링 없음). 실측 CPU 영향 ~0. M1 전에 본 문서로 확정 완료 |
| R2 | GNOME Wayland 감시 제약 | **사내 GNOME 버전 실사 전까지 미정**/상 | 버전 기준 전략(§6): **GNOME ≥49 = ext-data-control 1차 경로로 리스크 해소, GNOME ≤48(Ubuntu 22.04/24.04·RHEL9) = XWayland 폴백**(부재 런타임 감지+안내) → opt-in 폴링. M1에서 사내 표준 GNOME 버전 실사 최우선 — ≤48 표준이면 XWayland 필수 요건을 배포 문서에 명시 |
| R3 | Linux 트레이(appindicator/GNOME 확장) | 중/중 | 메뉴 중심 UX, 패키지 depends 명시, 창 열기 대체 경로, M1에서 실 데스크톱 VM 조기 검증 |
| R4 | 사내망 mDNS 차단(IGMP snooping/VLAN/AP isolation) | 중/상 | 수동 IP 추가 1급 기능(M2부터 존재), 마지막 IP 캐시 직접 재접속, 고정 포트 45871, **진단 패널(§8.1)로 원인 구분**, IT팀과 포트/멀티캐스트 사전 협의를 배포 문서에 포함 |
| R5 | 미서명 바이너리 배포 마찰(Gatekeeper/SmartScreen/사내 AV) | 중/중 | macOS는 Developer ID 1계정($99/yr) 확보가 최선(M3 착수), 차선 zip 배포+xattr 안내+MDM 예외. Windows는 사내 CA 서명+GPO 신뢰 루트 배포 |
| R6 | 에코 루프/OS 바이트 변형 루프 | 중/상 | 3중 방어(suppress+LRU+릴레이 금지) + PNG 정규화 해시 + Windows 제외 포맷. M2 완료 기준에 루프 테스트 명시 |
| R7 | 절전 복귀/네트워크 전환 후 mDNS·연결 사멸, **자기 IP 변경** | 중/중 | D24 플랫폼별 wake/인터페이스 변경 감지(수단·크레이트 확정) → 백오프 리셋 + **리스너 재바인딩 + mDNS 재등록(§4.5-5)** + 즉시 재연결, heartbeat 15s/45s. 이벤트 불가 환경은 60s 재열거 폴백 |
| R8 | Windows 클립보드 경합/지연 렌더링/알림 타이밍 | 중/중 | OpenClipboard 지수 백오프(100ms×10), 알림 후 50–100ms 지연 후 **동일 세션 원자 읽기**, 시퀀스 번호 디듀프, 타임아웃 있는 읽기+렌더 실패 조용히 스킵 |
| R9 | 일정 지연 연쇄(Wayland 직접 구현·서명 리드타임) | 중/중 | M1 3주 확장 + Wayland 스파이크 분리, wl-clipboard-rs 개작 지름길(D7), 20–30% 버퍼를 커밋 일정에 반영(§11), 서명은 M3 조기 착수 |
| R10 | GitHub ubuntu-22.04 러너 deprecation(2026-09 브라운아웃, 2027-04 제거) | 확실/중 | 러너 라벨과 glibc 하한 분리: **ubuntu-latest + `container: ubuntu:22.04`** 빌드로 처음부터 작성(§13.4), 컨테이너 내 의존성 설치 스크립트화. 사내 최저 배포판 실사 후 하한 24.04 상향 검토 |

---

## 13. 테스트 전략

### 13.1 유닛 테스트 (sb-*, CI 상시)
- 프로토콜: 프레임 인코드/디코드 round-trip(proptest), 잘린 프레임·과대 길이·미지 variant 거부, 전송 FSM 전이(Supersede/타임아웃/재요청 포함).
- 암호/페어링: mTLS 핸드셰이크 성공/미지 지문 실패, SPAKE2 코드 불일치 실패, **transcript 변조 감지(spake2 사용부 명시 포함)**, **동시 페어링 연결 시 두 번째 거부·시도 카운터 원자 소진(경쟁 시나리오)**, pinned 지문 변경 시 거부, trust store round-trip + **전용 MAC 키 무결성·변조 시 격리**.
- 네트워크: **주소 allowlist 판정(IPv4 사설/IPv6 ULA·링크로컬 zone id 허용, 전역 IPv4/IPv6 거부)**.
- 동기화: 에코 억제(suppress/LRU), LWW 수렴(동시 복사 시 전 노드 동일 승자 — property test), 크기 상한 거부(READ_HARD_LIMIT 포함), concealed 필터(마커 지연 채움 경쟁 포함), 고위험 패턴 판정.
- 저장: 암호화 round-trip, AAD 변조 감지, **keyed dedup 결정성·unkeyed 해시 미영속 확인**, retention/강등 정책, crypto-erase 후 복호 불가.
- 전제: 클립보드·탐색을 trait mock으로 대체해 코어 전체를 headless 테스트.

### 13.2 통합 테스트 (loopback 다노드)
- 한 테스트 바이너리에서 코어 노드(mock 클립보드, static discovery stub) 기동 → 페어링 → 양방향 텍스트/이미지 → 미승인 거부 → 재연결 → head 수렴. **3노드 구성 확장: 서로 다른 두 피어의 동시 신호 경합, 이미지 fetch 진행 중 더 높은 key 도착 시 Supersede/Abort 연쇄, 이후 전 노드 head 일치 확인**(full-mesh N≥3 전용 시나리오). mDNS 실검증은 flaky하므로 수동 테스트로 이관.
- **보안 회귀 스위트**(위협모델 위반 감지): 미페어 지문 핸드셰이크 거부, 틀린 코드 페어링 실패, 병렬 페어링 연결 거부, **전역 IPv4/IPv6 목적지 거부**, peers.json 변조(MAC 불일치) 격리, concealed 미전파, **로그 파일 내 클립보드 평문·페어링 코드 미포함**, **WebView에서 외부 호스트 fetch 차단(CSP)**, **외부 호스트 접속 0건(소켓 후킹 검증)**.
- 공급망: `cargo audit` + `cargo deny`(라이선스 게이트 포함 — GPL 검출) CI 필수 게이트, spake2 포함 감시.

### 13.3 3플랫폼 수동 체크리스트 (릴리스 게이트)
macOS / Win11 / Linux X11 / Wayland-KDE / Wayland-GNOME(≥49 및 ≤48 각각) 열에 대해: 설치→트레이 표시, mDNS 발견, 페어링 성공/오입력 실패, 텍스트 양방향(한글/emoji 포함), PNG 양방향 + **Windows DIB 왕복**, 연속 복사 루프 없음, sync off/on, 단절→자동 재연결, **Wi-Fi 전환(IP 변경) 후 자동 복구**, **초대형 이미지(100MB급) 복사 시 RSS<80MB 유지·정상 스킵**, 미승인 거부, 유휴 CPU <1%·RSS <80MB, 절전 복귀, autostart 재부팅, Wireshark 평문 미노출(릴리스당 1회).

### 13.4 개발 환경
- 맥 호스트 + VM: Ubuntu GNOME(Wayland, **GNOME ≤48 폴백 검증용**)/Fedora 최신(GNOME ≥49 1차 경로 검증용)/Ubuntu(X11)/Kubuntu(Wayland-KDE) 스냅샷(R2 검증 필수), Windows 11 ARM VM. **VM은 bridged 네트워킹 필수**(NAT는 멀티캐스트가 호스트 경계를 못 넘어 mDNS 테스트 불가). x86_64 최종 검증은 CI 빌드 산출물을 사내 실기기에서 스모크 테스트.
- CI: GitHub Actions(또는 사내 runner) 매트릭스 — Linux는 **ubuntu-latest 러너 + `container: ubuntu:22.04`**로 빌드해 glibc 하한(=지원 최저 배포판)을 러너 라벨과 분리 유지(R10: 22.04 러너 deprecation 대응) / macos(aarch64+x86_64) / windows. 컨테이너 내 libwebkit2gtk-4.1-dev, libayatana-appindicator3-dev 설치를 스크립트화. rust-cache + pnpm cache. 산출물 검증 절차에 deb depends(libwebkit2gtk-4.1-0, libayatana-appindicator3-1) 포함 확인.