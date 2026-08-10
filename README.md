# shareboard

**LAN-only, end-to-end encrypted clipboard sync for Linux, macOS and Windows.**

Copy on one machine, paste on another — inside your office network only. All content is encrypted
with a **group key that never leaves member devices**; the relay server sees ciphertext and
signatures only (a *blind relay*). No cloud, no telemetry, no outbound internet traffic.

Design rationale and the full specification live in [`PLAN.md`](./PLAN.md) (v2.0).

> **Note on language.** This README is in English, but the **desktop app UI and the server CLI
> output are currently Korean-only**. Every walkthrough below gives the exact Korean label followed
> by its English meaning, so you can follow along without reading Korean.

---

## Table of contents

- [What it is / what it is not](#what-it-is--what-it-is-not)
- [How it works](#how-it-works)
- [Requirements](#requirements)
- [Build from source](#build-from-source)
- [Usage guide](#usage-guide)
  - [Path A — Desktop app only (no terminal)](#path-a--desktop-app-only-no-terminal)
  - [Path B — Dedicated relay server + desktop apps](#path-b--dedicated-relay-server--desktop-apps)
  - [Inviting people](#inviting-people)
  - [Joining a workspace](#joining-a-workspace)
  - [Everyday use](#everyday-use)
  - [Removing a member (revocation)](#removing-a-member-revocation)
  - [Settings reference](#settings-reference)
  - [Running two instances on one machine](#running-two-instances-on-one-machine)
- [Running the standalone server](#running-the-standalone-server)
- [Security model](#security-model)
- [Protocol overview](#protocol-overview)
- [Repository layout](#repository-layout)
- [Development](#development)
- [Files and data locations](#files-and-data-locations)
- [Troubleshooting](#troubleshooting)
- [Project status](#project-status)
- [License](#license)

---

## What it is / what it is not

**It is:**

- A clipboard shared by a small, explicitly approved group of devices on one LAN.
- End-to-end encrypted: content, sort key and content kind are all sealed under a group key (GK).
- Text and PNG images, up to a configurable size limit (10 MiB by default).
- A tray-resident desktop app (Tauri 2 + Svelte 5) plus an optional headless relay binary.

**It is not:**

- Internet-reachable. Both the server bind address and the client dial address are checked against a
  **private-network allowlist**; a public IP is refused before a single byte is read.
- A file-transfer tool, a cloud clipboard, or a multi-tenant service. One server = one workspace.
- Production-signed software yet — see [Project status](#project-status).

---

## How it works

```
   ┌──────────────┐      ┌──────────────┐      ┌──────────────┐
   │   Client A   │      │   Client B   │      │   Client C   │
   │   (macOS)    │      │   (Linux)    │      │  (Windows)   │
   └──────┬───────┘      └──────┬───────┘      └──────┬───────┘
          │                     │                     │
          └─────────────────────┼─────────────────────┘
                                │   TLS 1.3 · mutual TLS
                                │   server fingerprint pinning
                     ┌──────────▼──────────┐
                     │      sb-server      │  relays ciphertext and
                     │    (blind relay)    │  signed log entries only —
                     └─────────────────────┘  never holds the group key,
                                              plaintext, or invite codes
```

1. **Membership is a hash-chained, member-signed workspace log.** The server only serialises appends;
   it cannot add a member, because every entry is signed and chain-verified by every client
   (`sb-crypto::wslog::verify_chain`).
2. **Joining** uses a one-time invite code (60 bits of entropy, Crockford Base32, stretched with
   Argon2id `m=64 MiB, t=3, p=1`). The invite blob **never contains the group key** — it carries only
   the single-use admission secret. Re-using a grant breaks chain verification, so an invite really is
   one-shot.
3. **The group key is delivered peer-to-peer through the relay**: an existing member wraps GK for the
   joiner's X25519 public key (ECDH-ES + HKDF + XChaCha20-Poly1305), signs it, and binds it to the
   adopted log entry hash and the member-set hash. The receiver runs `verify_rotation` against its own
   verified log before accepting — a malicious server cannot inject a key.
4. **On a clipboard change** the origin device sends only a small *signal* (content id, epoch,
   ciphertext size) plus the sealed body. Payloads at or under 32 KiB ride inline; larger ones are
   pulled on demand in 64 KiB chunks, so an idle 100-member workspace transfers almost nothing.
5. **Last-writer-wins** resolution with echo suppression keeps the loop from ping-ponging: a device
   that just applied a remote clip does not re-broadcast it.

---

## Requirements

| Need | Version | Notes |
|---|---|---|
| Rust | 1.82+ (stable) | `rust-version` floor in `Cargo.toml` |
| Node.js | 20+ (22 in CI) | frontend build only |
| pnpm | 11+ | Pinned by `packageManager` in `package.json`; `corepack enable pnpm` picks it up. `pnpm-workspace.yaml` uses the pnpm 11 `allowBuilds` syntax, so pnpm 9/10 cannot install |
| Tauri CLI | `^2` | `cargo install tauri-cli --version '^2'` |

**Linux build packages** (Debian/Ubuntu):

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libwayland-dev
```

macOS needs Xcode command line tools; Windows needs the MSVC toolchain and WebView2 (preinstalled on
Windows 11).

---

## Build from source

```bash
git clone <repo> shareboard && cd shareboard

# 1. Core crates — run the test suite first (fast, no GUI needed)
cargo test --workspace                 # 100 tests

# 2. Frontend dependencies
pnpm install

# 3a. Desktop app, development mode (hot reload)
cargo tauri dev

# 3b. Desktop app, distributable bundle → src-tauri/target/release/bundle/
cargo tauri build

# 4. Optional: headless relay server binary → target/release/sb-server
cargo build -p sb-server --release
```

The Tauri app is a **separate Cargo workspace** (`src-tauri/`, excluded from the root workspace) so
that GUI dependencies never slow down core-crate builds.

### Windows installers and executables

Windows artifacts are built by CI, because Tauri cannot cross-bundle for Windows from macOS or Linux
(it needs the MSVC linker, WebView2 and NSIS). Every push to `main` — and every manual **Actions →
build → Run workflow** — runs the `windows-bundle` job and uploads four files as the
`shareboard-windows-x64` artifact:

| File | Use |
|---|---|
| `shareboard_<ver>_x64-setup.exe` | **Installer (recommended).** Per-user install, so no administrator rights; registers the `shareboard://` deep link |
| `shareboard_<ver>_x64_en-US.msi` | Managed deployment (group policy / Intune); needs administrator rights |
| `shareboard-portable.exe` | Run without installing |
| `sb-server.exe` | Standalone relay binary |

Pushing a `v*` tag attaches the same four files to a GitHub Release. Pull requests skip the job — the
release LTO build is expensive — so they only get the `cargo check`.

On a Windows machine you can build the same set locally:

```powershell
pwsh -File scripts/build-windows.ps1        # → release/
pwsh -File scripts/build-windows.ps1 -NoMsi # installer only
```

Firewall rules, the SmartScreen warning (packages are unsigned), code-signing configuration and the
offline WebView2 option for air-gapped networks are covered in **[docs/WINDOWS.md](docs/WINDOWS.md)**.

---

## Usage guide

There are two ways to run a workspace. **Path A is the recommended starting point** — it needs no
terminal, no config file and no server machine.

### Path A — Desktop app only (no terminal)

One person's app hosts an embedded relay; everyone else connects to it. That machine must stay awake
with the app running.

**Host (person 1):**

1. Launch shareboard. Because no server is configured yet, the onboarding screen appears with three
   tabs: **이 기기가 서버** (*this device is the server*), **참여하기** (*join*), and
   **기존 서버에 만들기** (*create on an existing server*).
2. Stay on the first tab, **이 기기가 서버**.
3. Type a workspace name in **워크스페이스 이름** — *"Workspace name"* (e.g. `Design Team`).
4. Click **이 기기를 서버로 만들기** — *"Make this device the server"*.

The app now:
- picks your first private IPv4 address and binds the embedded relay to `<lan-ip>:45871`,
- generates the founder identity, the genesis log entry and group key `GK_0`,
- shows a card with **서버 주소** (*server address*) and **서버 지문** (*server fingerprint*), each with
  a 복사 (*copy*) button.

Hosting is remembered: on the next launch the app re-hosts automatically (`server.host = true` in
settings), and the workspace log survives restarts in `<data-dir>/server/wslog.cbor`.

**Everyone else:** see [Joining a workspace](#joining-a-workspace).

### Path B — Dedicated relay server + desktop apps

Use this when the relay should live on an always-on box (a NAS, a spare PC, a VM) instead of inside
someone's app. First set the server up as described in
[Running the standalone server](#running-the-standalone-server) — `sb-server --init` prints the three
values you need (address, fingerprint, setup token).

A fresh server starts *unclaimed*: exactly one person turns it into a workspace with the setup token.

**Founder (person 1):**

1. Launch shareboard and open the third onboarding tab, **기존 서버에 만들기** — *"Create on an
   existing server"*.
2. Fill in **서버 주소** (address, `host:port`), **서버 지문** (fingerprint, 64 hex chars),
   **워크스페이스 이름** (workspace name) and **setup 토큰** (the one-time setup token).
3. Click **워크스페이스 만들기** — *"Create workspace"*.

The app connects, claims the server with the token, and waits for the server's acknowledgement. If the
token is wrong or the server was already claimed, the failure is shown as **설정 실패: …** with the
server's reason (e.g. `토큰 불일치`, `이미 클레임됨`) and the app returns to onboarding — it does not
leave you with a half-created workspace.

The setup token is consumed conceptually at this point: the server refuses any further claim. Everyone
else joins with ordinary invites.

**Everyone else:** see [Joining a workspace](#joining-a-workspace) — identical to Path A, just pointing
at the standalone server's address and fingerprint.

### Inviting people

Any connected member with the group key can invite. Open the **멤버** (*Members*) tab:

| Button | Meaning | Result |
|---|---|---|
| **초대 링크 생성** | Generate invite link | `shareboard://join?a=<addr>&f=<fingerprint>&c=<code>` — one string that carries everything |
| **코드만 생성** | Generate code only | `XXXX-XXXX-XXXX` — must be sent together with address + fingerprint |

Both are valid for **1 hour** and are **single-use**. The invite link contains the code, so treat it
like a password: send it over an internal channel, not a public one.

### Joining a workspace

**With an invite link (easiest):**

1. Open shareboard → **참여하기** (*Join*) tab.
2. Paste the `shareboard://join?...` link into **초대 링크** (*Invite link*).
3. Click **참여** (*Join*).

Address, fingerprint and code are filled in automatically. The app connects, downloads and verifies
the log chain, appends its signed `Add` entry, and waits for the group key.

**By clicking the link:** the app registers the `shareboard://` URL scheme, so opening the link from a
messenger hands it straight to a running app, which joins and raises its window.

**Without a link:** on the same tab click **▼ 링크 없이 수동 입력** (*manual entry, no link*) and enter
**서버 주소** (address, `host:port`), **서버 지문** (fingerprint, 64 hex chars) and **초대 코드** (the
`XXXX-XXXX-XXXX` code), then **수동으로 참여** (*join manually*).

**What you should see afterwards** on the **개요** (*Overview*) tab:

| Banner | Meaning |
|---|---|
| `처리 중…` | Connecting, verifying the log, waiting for the key |
| `✅ 워크스페이스 준비됨 · 동기화 중` | Joined, group key present, syncing |
| `그룹 키 대기 중` | Joined, but the inviting member is offline; the key arrives automatically when they come back online |
| `설정 실패: …` | Join or creation failed (bad/expired code, wrong fingerprint, wrong setup token, server already claimed). The app clears the server setting and returns to onboarding so it does not retry-loop |

### Everyday use

Once joined, the app lives in the tray. Copying anything on any member device propagates to the rest
within a few hundred milliseconds.

**Tray menu:** **창 열기** (*Show window*) · **동기화 켬/끔** (*Toggle sync*) · **종료** (*Quit*).
Closing the window hides it rather than quitting.

**Tabs:**

- **개요** (*Overview*) — connection state, online/total members, history count, a sync on/off switch,
  and (when hosting) the shareable address + fingerprint.
- **멤버** (*Members*) — every member with an online dot, display name, connection address and short
  device id; invite generation; and the remove button for the founder.
- **히스토리** (*History*) — recent clips with kind tag, preview, origin, timestamp. **복사** re-copies
  an item to the OS clipboard (which re-propagates it), **핀**/📌 pins it, **✕** deletes it,
  **전체 삭제** clears everything.
- **설정** (*Settings*) — see below.

**How members are named:** each device seals a small profile `{name, platform, …}` under the group key
and publishes it, so other members see a human name rather than a hash — while the *server* still sees
only ciphertext. The name is your **OS username** by default, overridable in settings. The server also
stamps each session's connection address, shown next to the name.

**Clipboard change detection:** macOS uses the native `NSPasteboard.changeCount` — the contents are
read only when that counter moves — and also inspects the `org.nspasteboard.ConcealedType` /
`TransientType` hints so password-manager clips can be skipped. Linux and Windows compare clipboard
contents every 400 ms via `arboard`.

### Removing a member (revocation)

Only the **founder** sees the **내보내기** (*Remove*) button, and only while connected with a group key.
Clicking it (after the confirmation dialog) performs the full §4.4 sequence atomically:

1. Appends a signed `Remove` entry to the workspace log.
2. Appends an `Epoch` entry with `reason = Revoke` (grace period 0) bumping the epoch.
3. Generates a **brand-new random** `GK_{e+1}` — not derived from the old key, so it provides
   post-compromise security.
4. Wraps the new key for each *remaining* member, bound to the epoch entry hash and the new member-set
   hash, and sends it through the relay. Members who are offline get theirs from the server mailbox on
   their next `Welcome`.

The removed device is structurally excluded: it is no longer in the roster, so the relay does not fan
out to it, and it never receives `GK_{e+1}`. It keeps whatever plaintext it already had locally (there
is no remote wipe), but it can decrypt nothing new. Re-admission requires a fresh invite.

### Settings reference

Open the **설정** (*Settings*) tab and press **저장** (*Save*) to apply.

| Section | Field | Default | Effect |
|---|---|---|---|
| 연결 / 역할 (*Connection / role*) | role display + **서버/역할 다시 설정** | — | Shows whether this device hosts or is a client; the button clears the server config and returns to onboarding |
| 기기 이름 (*Device name*) | `device_name_override` | empty | Display name shown to other members; empty = OS username |
| 동기화 (*Sync*) | 텍스트 동기화 (`sync_text`) | on | Sync text clips |
| | 이미지 동기화 (`sync_images`) | on | Sync PNG clips |
| | 고위험 콘텐츠 확인 (`confirm_risky_content`) | on | *Declared, not yet enforced* |
| 히스토리 / 개인정보 | 디스크에 암호화 저장 (`persist_enabled`) | **off** | *Declared, not yet enforced* — history is in-memory today |
| | 비밀번호 매니저 콘텐츠 제외 (`exclude_concealed`) | on | Skips clips flagged concealed (macOS hint implemented) |
| | 인메모리 히스토리 최대 개수 (`memory_max_items`) | 30 | In-memory history cap |

Not exposed in the UI but present in `settings.json`: `max_content_bytes` (10 MiB, range 1–100 MiB),
`autostart`, `log_level`, `language`, `theme`, `excluded_apps`, `retention_days`. Fields marked
*declared, not yet enforced* round-trip through the file and the UI but do not change behaviour yet.

### Running two instances on one machine

Useful for demos and testing. Point each instance at its own data directory:

```bash
# Instance 1 — hosts the relay on <lan-ip>:45871
SHAREBOARD_DATA_DIR=/tmp/sb-a  ./shareboard

# Instance 2 — joins as a client (must NOT host; port 45871 is taken)
SHAREBOARD_DATA_DIR=/tmp/sb-b  ./shareboard
```

Each gets its own identity, keys, settings and history. Both share the one real OS clipboard, so for a
convincing end-to-end demo prefer the scripted examples, which give each simulated client its own
in-memory clipboard:

```bash
cargo run -p sb-server --example two_client_sync      # invite → join → GK delivery → 2-way sync
cargo run -p sb-server --example three_client_revoke  # 4 members, revoke one, key rotation, offline catch-up
```

---

## Running the standalone server

*(The Korean version of this section, with systemd details, is in [`docs/SERVER.md`](docs/SERVER.md).)*

There are exactly three values to pass around:

- **Server address** — where apps connect, e.g. `192.168.0.10:45871`
- **Server fingerprint** — SHA-256 of the server's TLS SPKI; how apps verify they reached *your*
  server. Announce it to everyone.
- **Setup token** — one-time value used by the single person who creates the workspace.

**One-shot setup:**

```bash
# Local testing (apps on the same machine)
sb-server --init

# Real deployment — use the server machine's LAN IP
sb-server --init --bind 192.168.0.10:45871
```

This writes `server.toml`, creates the server identity in `data_dir/identity.bin` (mode 0600) and
prints the address, the fingerprint and the setup token. Then start it:

```bash
sb-server --config server.toml
```

> The fingerprint is derived from the stored identity, so it is **stable across restarts**. Deleting
> `data_dir` regenerates the identity and therefore changes the fingerprint, which invalidates every
> client's pin.

The workspace log is persisted to `data_dir/wslog.cbor`, so **membership survives a restart** — on
start-up the server reports `기존 워크스페이스 로그를 …/wslog.cbor 에서 복원했습니다` and refuses a
second claim. Invites, presence and the key mailbox are in-memory and are rebuilt by clients on
reconnect; an invite issued moments before a restart has to be re-issued.

**`server.toml`:**

```toml
bind_addr = "192.168.0.10:45871"   # must be a private-network address
data_dir  = "./sb-server-data"     # identity.bin + wslog.cbor — back this up
setup_token_hash = "d8be07…"       # SHA-256 of the setup token; the plaintext is never stored
```

`log_dir` and `health_bind` are also accepted by the config parser but are not implemented.
Only private ranges are accepted for `bind_addr` (`127/8`, `10/8`, `172.16/12`, `192.168/16`,
`169.254/16`, IPv6 `::1`, `fc00::/7`, `fe80::/10`). A public IP makes the server refuse to start. Find
your LAN IP with `ipconfig getifaddr en0` (macOS), `ip -4 addr` (Linux) or `ipconfig` (Windows).

Other flags: `--gen-token` prints a fresh setup token and its hash without touching config;
`--data-dir` overrides the identity/log directory for `--init`.

**Keeping it running (systemd):**

```ini
[Unit]
Description=shareboard relay
After=network.target

[Service]
ExecStart=/usr/local/bin/sb-server --config /etc/shareboard/server.toml
Restart=on-failure
# Kernel-level second lock: LAN only
IPAddressAllow=192.168.0.0/16 127.0.0.0/8
IPAddressDeny=any

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now shareboard
```

**Firewall:** open exactly **one inbound TCP port on the server machine** (45871 by default). Clients
never listen, so they need no inbound rules at all.

---

## Security model

| Property | Mechanism |
|---|---|
| Transport | TLS 1.3 with mutual TLS. The client pins the server by SHA-256 of its SPKI instead of trusting a CA chain; the handshake signature is still verified, so pinning proves identity *and* key possession. |
| Content confidentiality | Body, sort key and content kind are sealed under the epoch group key with XChaCha20-Poly1305 (24-byte random nonce). The relay only ever sees `{content id, epoch, ciphertext size}`. |
| Device identity | P-256 for TLS and log signing, X25519 for key wrapping. `device_id = SHA-256(SPKI)`, and the `Hello` device id must match the presented TLS certificate. |
| Membership | Hash-chained log; each entry verified for signature, previous-hash linkage, sponsor membership, grant expiry, and single-use grants. The server cannot forge membership because it holds no member key. |
| Admission | 60-bit invite code → Argon2id (64 MiB, t=3) → locator + sealing key. The blob holds the admission secret, never the group key. Codes are one-hour, one-use. |
| Key delivery | Signed rotation blob bound to `(epoch, reason, adopted entry hash, member-set hash)`, sealed per-recipient. Receivers run `verify_rotation` against their own verified log, so a hostile relay cannot inject or downgrade a key. |
| Revocation | New random key at each epoch (post-compromise security), grace period 0 for revocations, fan-out restricted to the post-removal roster. |
| Network exposure | Private-address allowlist enforced on server bind *and* client dial. Global IPv6 is explicitly rejected. Zero outbound internet traffic, zero telemetry. |
| Unauthenticated peers | Certificates not present in the log land in a restricted **guest lane** that accepts only four message types (`ClaimWorkspace`, `GetInviteBlob`, `GetLog`, `AppendEntry`); anything else ends the session. (The spec's tighter 32 KiB guest frame cap and per-IP guest quotas are defined but not yet applied.) |
| Secrets at rest | Identity, group key, history key, dedup key and workspace MAC key are stored as 0600 files in `<data-dir>/keys/` (directory 0700). *OS keychain storage is specified but not yet wired* — see [Project status](#project-status). |
| Privacy defaults | History disk persistence off, concealed-clipboard exclusion on, non-identifying fallback device name. |

---

## Protocol overview

CBOR-encoded `Envelope<C2s>` / `Envelope<S2c>` frames over TLS. Protocol version 2 (`PROTO_MIN =
PROTO_MAX = 2`).

**Client → server:** `Hello` · `ClaimWorkspace` · `GetInviteBlob` · `GetLog` · `AppendEntry` ·
`PutInvite` · `RevokeInvite` · `PutKeyUpdate` · `ClipSignal` · `ContentRequest` · `ContentBegin` ·
`ContentChunk` · `ContentAbort` · `ContentReject` · `SetProfile` · `Leave` · `Ping` · `Bye`

**Server → client:** `Welcome` · `InviteBlob` · `LogEntries` · `AppendAck` · `AppendReject` ·
`SignalFanout` · `ContentPull` · `ContentBegin` · `ContentChunk` · `ContentReject` · `ContentAbort` ·
`LogAppended` · `KeyUpdatePush` · `Presence` · `Revoked` · `Pong` · `Error` · `Bye`

Selected parameters (`sb-proto::params`). The whole §5.6 parameter table is defined as constants, but
not every limit has a call site yet — the last column says which are live today:

| Parameter | Value | Enforced |
|---|---|---|
| Default port | 45871 | yes |
| Max frame | 256 KiB | yes |
| Guest-lane max frame | 32 KiB | no — the guest lane restricts *message types*, not frame size |
| Inline threshold | 32 KiB | yes |
| Chunk size | 64 KiB | yes |
| Max content size | 10 MiB default (1–100 MiB) | yes |
| Hard read limit | 32 MiB | yes |
| Invite code / TTL | 60 bits, 12 Crockford Base32 chars / 1 h | yes |
| Argon2id | m = 64 MiB, t = 3, p = 1 | yes |
| Suppress grace | 2 s | yes |
| Head cache depth | 4 signals | yes |
| Clipboard poll interval | 400 ms (app worker) | yes |
| Debounce | 150 ms | no |
| Heartbeat / dead / session idle | 15 s / 45 s / 90 s | no |
| Reconnect backoff | 1 s → 30 s, 20 % jitter | no — the worker retries on a flat 3 s |
| Signal rate limit | 10/s per client | no |
| Connection caps (member / guest) | 64 / 8 | no |
| Fetch timeout | 10 s | no |

---

## Repository layout

| Path | Role |
|---|---|
| `crates/sb-proto` | Wire messages, E2E payloads, log entry types, LAN allowlist, protocol parameters |
| `crates/sb-crypto` | Identity (P-256 + X25519), group keys (XChaCha20-Poly1305), invites (Argon2id), key wrapping, log hash-chain verification |
| `crates/sb-core` | Sync engine: LWW resolution, echo suppression, E2E seal/open, settings, in-memory history |
| `crates/sb-net` | TLS 1.3 mTLS client, fingerprint pinning verifier, framing |
| `crates/sb-server` | Blind relay: library + `sb-server` binary + runnable demos |
| `crates/sb-store` | Encrypted history (rusqlite with field encryption), key store, key manager |
| `crates/sb-clipboard` | `ClipboardAccess` trait, change watchers, mock + arboard backends, macOS native detection, Wayland scaffold |
| `src-tauri` | Tauri 2 desktop app: tray, commands, background worker, embedded relay |
| `src` | Svelte 5 UI (`App.svelte`, `lib/ipc.ts`) |
| `docs/SERVER.md` | Server setup guide (Korean) |
| `PLAN.md`, `PLAN-v1.1-p2p.md` | Full specification (v2.0) and the superseded P2P design |
| `scripts/` | `gen-icons.sh` (icon pipeline), `linux-verify.sh` (Docker Linux verification) |

---

## Development

```bash
cargo test --workspace          # 100 tests
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo build -p sb-clipboard --features wayland-backend   # Linux only, off by default

# Runnable end-to-end demos (real TLS server, real crypto, simulated clipboards)
cargo run -p sb-server --example two_client_sync
cargo run -p sb-server --example three_client_revoke

# Transport smoke test against a running sb-server: TLS + pinning + mTLS + claim + Welcome.
# Uses a throwaway identity and exits — it does NOT set up a usable workspace.
cargo run -p sb-server --example smoke_client -- <addr> <fingerprint-hex> <setup-token>

# Verify the Linux build path from macOS
docker run --rm -v "$PWD":/w -w /w ubuntu:24.04 bash scripts/linux-verify.sh

# Regenerate icons from the master SVG (needs resvg + tauri-cli)
./scripts/gen-icons.sh
```

**Test distribution:** `sb-crypto` 36 · `sb-core` 22 · `sb-proto` 16 · `sb-store` 14 · `sb-clipboard` 5 ·
`sb-server` 4 · `sb-net` 3. The `sb-server` set includes an integration test where two clients complete
a full E2E sync through a real relay, and `sb-net` stands up an actual TLS server on loopback.

**Icons:** `assets/icons/app-icon.svg` is the master. `scripts/gen-icons.sh` renders it at 1024 px with
resvg, feeds `cargo tauri icon` for the platform sets, and emits tray PNGs at 16/22/32/44 px as macOS
template images (monochrome + alpha).

**CI** (`.github/workflows/build.yml`):

| Job | What it does |
|---|---|
| `test` | 3-OS matrix: `fmt --check`, `clippy --all-targets`, `cargo test --workspace`, plus a Wayland-backend compile check on Linux |
| `server-build` | Release build of `sb-server` inside an `ubuntu:22.04` container, pinning the glibc floor below the runner's; uploads the binary as an artifact |
| `frontend` | `pnpm install --frozen-lockfile && pnpm build` |
| `app-check` | 3-OS `cargo check` of `src-tauri` |
| `windows-bundle` | Runs `scripts/build-windows.ps1` on `windows-latest`: installer, MSI, portable exe and `sb-server.exe` as an artifact; attaches them to a Release on `v*` tags. Skipped on pull requests |
| `supply-chain` | `cargo-deny check advisories licenses bans sources` |

`deny.toml`'s allow-list contains no GPL-family licences, so a GPL dependency fails the build
automatically.

---

## Files and data locations

**Desktop app** — `<data-dir>/`, where `<data-dir>` is `$SHAREBOARD_DATA_DIR` if set, otherwise:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/shareboard` |
| Linux | `$XDG_DATA_HOME/shareboard` or `~/.local/share/shareboard` |
| Windows | `%APPDATA%\shareboard` |

| File | Contents |
|---|---|
| `settings.json` | All settings, including server address, pinned fingerprint and host flag |
| `keys/*.key` | Identity (signing + KEM), current group key, history key, dedup key, workspace MAC key — 0600 files in a 0700 directory |
| `history.db` | SQLite history store with field-level encryption (created at startup; unused while persistence is unwired) |
| `server/wslog.cbor` | Workspace log, only when this device hosts the embedded relay |

**Standalone server** — `data_dir` from `server.toml` (default `./sb-server-data`):

| File | Contents |
|---|---|
| `identity.bin` | Server identity (0600) — determines the fingerprint clients pin |
| `wslog.cbor` | Workspace membership log, rewritten on every accepted append |

Back this directory up. Losing `identity.bin` forces a new fingerprint on every client; losing
`wslog.cbor` returns the server to the unclaimed state and everyone has to re-found and re-join.

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Server refuses to start: `bind_addr … LAN allowlist 밖` | You used a public IP. Use a private-range address. |
| App never connects | Open inbound TCP 45871 on the server machine's firewall; check both are on the same subnet. |
| Header stays `연결 끊김` (disconnected) and retries forever | Most often a fingerprint mismatch: pinning fails during the TLS handshake, and the app surfaces no specific message — it just reconnects every 3 s. Usually the server's `data_dir` was deleted, generating a new identity; redistribute the new fingerprint. Run with `RUST_LOG=debug` to see the real `서버 연결 실패` cause. |
| Join fails right after pasting a link | Code expired (1 h) or already used. Ask for a fresh invite. The app resets to onboarding on failure by design. |
| `설정 실패: 토큰 불일치` when creating on a standalone server | The setup token does not match the server's `setup_token_hash`. Re-running `sb-server --init` mints a new token, so use the one from the *current* config. |
| `설정 실패: 이미 클레임됨` | That server already hosts a workspace — a server holds exactly one. Join it with an invite instead, or point `--init` at a fresh `data_dir` for a separate workspace. |
| `그룹 키 대기 중` never clears | The member who invited you is offline. The key is delivered automatically the moment they reconnect; any other online member's invite works too. |
| Hosting fails with a bind error | Port 45871 is already in use — often a second instance on the same machine. Only one instance per machine can host. |
| Clipboard changes not picked up on Linux | Wayland native event watching is not implemented yet; the X11/XWayland `arboard` path polls every 400 ms. Under a strict Wayland-only compositor, detection may be unreliable. |
| History shows items but 복사 is disabled | The body is no longer in the in-memory cache (it is bounded). Only cached items can be re-copied. |
| Two local instances share clips oddly | They share one OS clipboard. Use the `two_client_sync` example for a clean demo. |

Set `RUST_LOG=debug` (or `sb_server=debug`) before launching for verbose tracing on either binary.

---

## Project status

All seven core crates are complete and tested (**100 tests**, green on macOS, Linux and Windows in
CI). The desktop app wires them together and runs; the relay carries a real two-client E2E sync
integration test, and the revocation path has a live four-member demo.

**Done**

- Blind relay with membership from a verified log, guest-lane message restriction, presence with
  server-stamped addresses, a mailbox for offline key delivery, and opt-in log persistence (used by
  the app's embedded relay).
- Full join flow: invite link / code / `shareboard://` deep link, chain verification, signed and
  bound group-key delivery with `verify_rotation`.
- Member removal with epoch rotation and re-wrapped keys, including offline catch-up ordering
  (log first, then wrap) verified by the demo.
- Embedded relay hosting from the UI — no terminal needed to start a workspace.
- Founding a workspace on a **standalone `sb-server`** from the UI (third onboarding tab), with the
  server's claim result actually checked: a wrong or already-used setup token surfaces the server's
  reason instead of silently leaving a workspace the server never accepted.
- Workspace-log persistence in the standalone server — membership survives a restart.
- macOS `changeCount` detection plus `org.nspasteboard.ConcealedType` exclusion.
- CI across three platforms, `cargo-deny` supply-chain gate, rustfmt, clippy.
- Wayland data-control backend **scaffold** (`wayland-backend` feature, Linux only, off by default),
  compile-verified in Docker (`ubuntu:24.04`, `wl-clipboard-rs` 0.9.3).

**Known limitations / next up**

- **OS keychain storage is not wired.** Keys live in 0600 files; the fallback-ladder design exists but
  the `keyring` backend is not injected yet.
- **History disk persistence is not wired.** `history.db` is created and the toggle round-trips, but
  clips are only kept in memory, so history is lost on quit.
- **Settings declared but not enforced:** `confirm_risky_content`, `auto_apply_received`,
  `excluded_apps`, `exclude_patterns`, `retention_days`, `store_image_originals`, `autostart`,
  `language`, `theme`.
- **Abuse limits not enforced yet:** per-client signal rate limiting, connection caps, guest-lane
  frame/quota limits, heartbeat and fetch timeouts, and exponential reconnect backoff are all
  specified in `sb-proto::params` but have no call site — the client currently retries on a flat 3 s.
- **Server-side invites and presence are still in-memory**, so an invite issued just before a server
  restart must be re-issued (the membership log itself now persists). `log_dir` and `health_bind` in
  `server.toml` are parsed but unimplemented.
- **Wayland native event watching** (`data-control` selection subscription) is still polling, and
  needs verification on real Linux hardware.
- **Concealed-content detection is macOS-only**; Linux and Windows hints are not read yet.
- **UI is Korean-only** — no i18n layer yet.
- **No signed packages.** Windows installers and executables are now built automatically by CI, but
  they are **unsigned** (SmartScreen warns on first run); macOS/Linux bundles are neither built by CI
  nor signed or notarised. See [docs/WINDOWS.md](docs/WINDOWS.md) for the signing configuration.

---

## License

Dual-licensed under **MIT OR Apache-2.0**, as declared in `Cargo.toml`. (Full licence texts are not
yet checked into the repository.)
