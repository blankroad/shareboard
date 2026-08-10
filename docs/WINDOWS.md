# Windows 배포물 만들기 / 설치하기

Windows 산출물은 4개다.

| 파일 | 용도 |
|---|---|
| `shareboard_<ver>_x64-setup.exe` | **설치 파일(권장)**. 현재 사용자 설치 — 관리자 권한 불필요, 시작 메뉴 등록, `shareboard://` 딥링크 등록 |
| `shareboard_<ver>_x64_en-US.msi` | MSI. 그룹 정책/Intune 같은 관리 배포용(설치 시 관리자 권한 필요) |
| `shareboard-portable.exe` | 설치 없이 바로 실행. USB·임시 검증용 |
| `sb-server.exe` | 릴레이 서버 단독 바이너리 (앱 UI 의 "이 기기를 서버로" 대신 서버만 상주시킬 때) |

---

## 1. 받는 방법 — CI 산출물 (Windows PC 없어도 됨)

`main` 푸시·수동 실행 때마다 `windows-bundle` 잡이 위 4개를 만든다.

1. GitHub → **Actions** → `build` 워크플로 → 해당 실행 클릭
2. 아래 **Artifacts** → `shareboard-windows-x64` 다운로드 (zip)

수동으로 뽑기: Actions → `build` → **Run workflow**.

PR 에서는 무거운 릴리스 LTO 빌드를 건너뛴다(컴파일 체크만 함).

### 버전 배포 (태그)

```bash
git tag v0.1.0 && git push origin v0.1.0
```

`v*` 태그를 푸시하면 같은 파일들이 **GitHub Release** 에 자동 첨부된다.
`src-tauri/tauri.conf.json` 의 `version` 과 태그를 같이 올리는 걸 잊지 말 것(설치 파일 이름·업그레이드 판정에 쓰인다).

---

## 2. Windows PC 에서 직접 빌드

사전 준비:

- **Rust** (MSVC 툴체인): <https://rustup.rs>
- **Visual Studio Build Tools** — "C++를 사용한 데스크톱 개발" 워크로드
- **Node 22 + pnpm** (`npm i -g pnpm`)
- WebView2 런타임 — Windows 11 및 최신 Windows 10 에는 이미 있음

```powershell
git clone <repo> ; cd shareboard
pwsh -File scripts/build-windows.ps1
```

산출물은 `release/` 에 모인다. 옵션:

```powershell
pwsh -File scripts/build-windows.ps1 -NoMsi        # 설치 파일(NSIS)만
pwsh -File scripts/build-windows.ps1 -SkipInstall  # pnpm install 생략(CI 용)
```

> **macOS/Linux 에서는 만들 수 없다.** Tauri 의 Windows 번들러는 MSVC 링커·WebView2·NSIS 가
> 필요해 크로스 번들링을 지원하지 않는다. 다른 OS 에서 작업 중이면 위 1번(CI)을 쓴다.

---

## 3. 설치 후 확인할 것

- **방화벽** — 이 PC 가 서버 역할(앱의 "이 기기를 서버로" 또는 `sb-server.exe`)이면 첫 실행 때
  Windows Defender 방화벽 알림에서 **개인 네트워크** 허용. 놓쳤으면:
  `제어판 → Windows Defender 방화벽 → 앱 허용`, 또는 기본 포트 **45871/TCP** 인바운드 허용.
  클라이언트로만 쓸 경우 인바운드 허용은 필요 없다.
- **딥링크** — 설치 파일이 `shareboard://` 스킴을 레지스트리에 등록한다. 포터블 exe 는 실행 시
  스스로 등록을 시도하지만(best-effort), 초대 링크 클릭이 안 되면 앱의 "참여하기"에 링크를
  붙여넣으면 된다.
- **데이터 위치** — `%APPDATA%\shareboard` (`SHAREBOARD_DATA_DIR` 로 변경 가능. 한 PC 에서
  2개 인스턴스를 띄워 시연할 때 사용).

### SmartScreen 경고

코드 서명 인증서를 아직 붙이지 않아서 "Windows에서 PC를 보호했습니다" 경고가 뜬다
(**추가 정보 → 실행**). 사내 배포용으로 없애려면 EV/OV 코드 서명 인증서를 준비한 뒤
`src-tauri/tauri.conf.json` 의 `bundle.windows` 에 다음을 추가한다:

```json
"certificateThumbprint": "<인증서 SHA1 지문>",
"digestAlgorithm": "sha256",
"timestampUrl": "http://timestamp.digicert.com"
```

CI 에서 서명하려면 인증서를 시크릿으로 올리고 `windows-bundle` 잡에서 인증서 저장소에
임포트하는 단계를 추가해야 한다.

### 인터넷이 막힌 사내망이라면

기본 설정은 WebView2 가 없는 PC 에서 설치 중 부트스트래퍼를 **다운로드**한다
(`webviewInstallMode: downloadBootstrapper`). 완전 폐쇄망이면 오프라인 설치본을 내장한다 —
`src-tauri/tauri.conf.json`:

```json
"webviewInstallMode": { "type": "offlineInstaller", "silent": true }
```

설치 파일이 약 130MB 커진다. 대상 PC 가 Windows 11 이거나 WebView2 가 이미 있으면 기본값으로 충분하다.

---

## 4. 서버만 상주시키기 (`sb-server.exe`)

```powershell
.\sb-server.exe --init --bind 192.168.0.10:45871   # server.toml + 토큰 + 지문 생성
.\sb-server.exe --config server.toml               # 기동
```

설정 항목·문제 해결은 [SERVER.md](./SERVER.md) 참고. 서비스로 상주시키려면 작업 스케줄러
("시스템 시작 시" 트리거, 최고 권한) 또는 [NSSM](https://nssm.cc) 을 쓴다.
