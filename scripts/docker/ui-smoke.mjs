// 실제 shareboard 앱(Tauri + webkit2gtk)을 WebDriver 로 조작하는 UI 스모크.
//
// 모의 IPC 가 아니라 **진짜 Rust 백엔드**가 붙은 웹뷰를 띄워 클릭·입력하고, 화면에 무엇이
// 그려졌는지로 판정한다. 의존성 없이 W3C WebDriver 엔드포인트를 fetch 로 직접 호출한다.
//
//   single: 온보딩 3모드 → 호스팅 → 개요/멤버/히스토리/설정 → 초대 → 실제 X11 클립보드 캡처
//   two   : A 호스트 → 초대 링크 → B 참여 → 양방향 클립보드 동기화 → 강퇴 후 접근 상실
import { writeFileSync, mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";

const OUT = process.env.OUT ?? "/out";
const SHOTS = `${OUT}/shots`;
mkdirSync(SHOTS, { recursive: true });

const checks = [];
let fatal = null;

function ok(name, pass, detail = "") {
  checks.push({ name, pass: !!pass, detail });
  const mark = pass ? "✅" : "❌";
  console.log(`  ${mark} ${name}${detail ? ` — ${detail}` : ""}`);
  return !!pass;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Driver {
  constructor(base, tag, display) {
    this.base = base;
    this.tag = tag;
    this.display = display;
    this.sid = null;
  }

  async req(method, path, body, timeoutMs = 30000) {
    const res = await fetch(this.base + path, {
      method,
      headers: { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
    const raw = await res.text();
    let json;
    try {
      json = JSON.parse(raw);
    } catch {
      json = null;
    }
    if (!res.ok || (json && json.value && json.value.error)) {
      const e = json?.value?.error
        ? `${json.value.error}: ${json.value.message ?? ""}`
        : `HTTP ${res.status} ${raw.slice(0, 400)}`;
      throw new Error(`[${this.tag}] ${method} ${path} → ${e}`);
    }
    return json ? json.value : null;
  }

  async start(app) {
    const caps = {
      capabilities: {
        alwaysMatch: { "tauri:options": { application: app, args: [] } },
      },
      // 구형 JSON-wire 를 기대하는 브리지 대비.
      desiredCapabilities: { "tauri:options": { application: app, args: [] } },
    };
    const v = await this.req("POST", "/session", caps, 120000);
    this.sid = v.sessionId ?? v.capabilities?.sessionId;
    if (!this.sid) throw new Error(`[${this.tag}] 세션 ID 를 못 받았다: ${JSON.stringify(v).slice(0, 300)}`);
    return this.sid;
  }

  js(script, args = [], timeoutMs = 30000) {
    return this.req("POST", `/session/${this.sid}/execute/sync`, { script, args }, timeoutMs);
  }

  async shot(name) {
    try {
      const b64 = await this.req("GET", `/session/${this.sid}/screenshot`);
      writeFileSync(`${SHOTS}/${name}.png`, Buffer.from(b64, "base64"));
      console.log(`  📸 webview:${name}.png`);
    } catch (e) {
      console.log(`  (스크린샷 실패 ${name}: ${e.message})`);
    }
    // 창 테두리·트레이까지 보이는 전체 화면도 남긴다.
    if (this.display) {
      spawnSync("import", ["-display", this.display, "-window", "root", `${SHOTS}/${name}-screen.png`]);
    }
  }

  handles() {
    return this.req("GET", `/session/${this.sid}/window/handles`);
  }
  switchTo(h) {
    return this.req("POST", `/session/${this.sid}/window`, { handle: h });
  }
  quit() {
    return this.req("DELETE", `/session/${this.sid}`).catch(() => {});
  }

  // ── DOM 헬퍼 (전부 execute/sync 로 처리 — 한글 라벨 매칭이 CSS 보다 안전하다)
  text(sel) {
    return this.js(
      `const e = document.querySelector(arguments[0]); return e ? e.textContent.trim() : null;`,
      [sel],
    );
  }
  bodyText() {
    return this.js(`return document.body.innerText;`);
  }
  count(sel) {
    return this.js(`return document.querySelectorAll(arguments[0]).length;`, [sel]);
  }
  texts(sel) {
    return this.js(
      `return [...document.querySelectorAll(arguments[0])].map(e => e.textContent.trim());`,
      [sel],
    );
  }
  /// 텍스트로 버튼을 찾아 클릭. 못 찾으면 후보 목록을 돌려준다(디버깅용).
  clickText(label, sel = "button") {
    return this.js(
      `const t = arguments[0];
       const els = [...document.querySelectorAll(arguments[1])];
       const el = els.find(e => e.textContent.replace(/\\s+/g,' ').trim().includes(t));
       if (!el) return { ok:false, candidates: els.map(e => e.textContent.replace(/\\s+/g,' ').trim()) };
       if (el.disabled) return { ok:false, disabled:true };
       el.click();
       return { ok:true };`,
      [label, sel],
    );
  }
  /// placeholder 로 input 을 찾아 값을 넣는다. Svelte bind 가 반응하도록 input 이벤트를 쏜다.
  fill(placeholderPart, value) {
    return this.js(
      `const [p, v] = arguments;
       const el = [...document.querySelectorAll('input')].find(i => (i.placeholder||'').includes(p));
       if (!el) return { ok:false, placeholders: [...document.querySelectorAll('input')].map(i => i.placeholder) };
       el.focus(); el.value = v;
       el.dispatchEvent(new Event('input', { bubbles: true }));
       el.dispatchEvent(new Event('change', { bubbles: true }));
       return { ok:true };`,
      [placeholderPart, value],
    );
  }
  inputValue(placeholderPart) {
    return this.js(
      `const el = [...document.querySelectorAll('input')].find(i => (i.placeholder||'').includes(arguments[0]));
       return el ? el.value : null;`,
      [placeholderPart],
    );
  }
  disabledOf(label) {
    return this.js(
      `const el = [...document.querySelectorAll('button')].find(e => e.textContent.trim().includes(arguments[0]));
       return el ? !!el.disabled : null;`,
      [label],
    );
  }
  /// 라벨이 **정확히** 일치하는 버튼 클릭 — "참여" 는 탭 "참여하기" 와 부분일치하므로 필요.
  clickTextExact(label, sel = "button") {
    return this.js(
      `const els = [...document.querySelectorAll(arguments[1])];
       const el = els.find(e => e.textContent.replace(/\\s+/g,' ').trim() === arguments[0]);
       if (!el) return { ok:false, candidates: els.map(e => e.textContent.replace(/\\s+/g,' ').trim()) };
       if (el.disabled) return { ok:false, disabled:true };
       el.click();
       return { ok:true };`,
      [label, sel],
    );
  }
  /// 라벨이 **정확히** 일치하는 버튼의 disabled — "참여" 가 탭 버튼 "참여하기" 와 겹치는 경우용.
  disabledOfExact(label) {
    return this.js(
      `const el = [...document.querySelectorAll('button')].find(e => e.textContent.trim() === arguments[0]);
       return el ? !!el.disabled : null;`,
      [label],
    );
  }
  async poll(desc, fn, timeoutMs = 20000, every = 400) {
    const t0 = Date.now();
    let last;
    while (Date.now() - t0 < timeoutMs) {
      try {
        last = await fn();
        if (last) return last;
      } catch (e) {
        last = `예외: ${e.message}`;
      }
      await sleep(every);
    }
    throw new Error(`대기 시간 초과: ${desc} (마지막 값: ${JSON.stringify(last)?.slice(0, 200)})`);
  }
  installErrorHook() {
    return this.js(`
      window.__ui_errors = window.__ui_errors || [];
      if (!window.__hooked) {
        window.__hooked = true;
        window.addEventListener('error', e => window.__ui_errors.push('error: ' + (e.message || e.error)));
        window.addEventListener('unhandledrejection', e => window.__ui_errors.push('rejection: ' + e.reason));
        const ce = console.error;
        console.error = (...a) => { window.__ui_errors.push('console.error: ' + a.map(String).join(' ')); ce(...a); };
      }
      return true;`);
  }
  uiErrors() {
    return this.js(`return window.__ui_errors || [];`);
  }
  /// main.ts 는 어떤 런타임 오류든 #app 을 <pre> 로 덮는다 — 그게 있으면 화면이 죽은 것이다.
  crashPane() {
    return this.js(`const p = document.querySelector('#app > pre'); return p ? p.textContent.slice(0, 500) : null;`);
  }
}

// ── X11 클립보드 (디스플레이별로 독립)
/// xclip 은 selection 소유권을 쥐려고 fork 해 계속 살아 있는다. 자식의 stdout 을 파이프로
/// 물리면 부모가 EOF 를 기다리다 영영 멈추므로 반드시 ignore 로 띄운다.
function clipSet(display, text) {
  const r = spawnSync("xclip", ["-display", display, "-selection", "clipboard", "-i"], {
    input: text,
    stdio: ["pipe", "ignore", "ignore"],
    timeout: 10000,
  });
  if (r.error) throw new Error(`xclip 실패(${display}): ${r.error.message}`);
}
/// xclip 의 기본 타깃은 STRING 인데 arboard 는 UTF8_STRING 계열만 광고한다 —
/// UTF8_STRING 을 먼저 물어봐야 "붙여넣기 되는 상태"를 정확히 본다.
function clipGet(display) {
  for (const t of ["UTF8_STRING", "text/plain;charset=utf-8", "STRING"]) {
    const r = spawnSync(
      "xclip",
      ["-display", display, "-selection", "clipboard", "-o", "-t", t],
      { encoding: "utf8", timeout: 5000 },
    );
    if (r.status === 0 && (r.stdout ?? "").trim().length) return r.stdout.trim();
  }
  return "";
}

// ── 공통 시나리오 조각 ────────────────────────────────────────────────

async function waitBoot(d) {
  // 앱은 창을 둘 만든다(main + 숨겨진 quick 팝업). WebDriver 세션의 기본 창이 quick 일 수 있어
  // location.hash 로 구분해 main 으로 옮긴다 — 안 하면 "웹뷰가 안 뜬다"로 오진한다.
  const handles = (await d.handles()) ?? [];
  for (const h of handles) {
    await d.switchTo(h);
    const hash = await d.js(`return location.hash;`).catch(() => "?");
    if (hash === "#quick") d.quickHandle = h;
    else d.mainHandle = h;
  }
  ok(`${d.tag}: 창 2개(main+quick) 생성`, handles.length === 2 && !!d.mainHandle, `handles=${handles.length}`);
  if (d.mainHandle) await d.switchTo(d.mainHandle);

  await d.poll(
    `${d.tag}: 웹뷰 로드`,
    async () => (await d.js(`return !!document.querySelector('header .brand');`)) === true,
    60000,
  );
  const url = await d.js(`return location.href;`);
  ok(`${d.tag}: 배포용 자산 로드(tauri:// 프로토콜)`, String(url).startsWith("tauri://"), String(url));
  await d.installErrorHook();
  const crash = await d.crashPane();
  ok(`${d.tag}: 부팅 시 런타임 오류 없음`, crash === null, crash ? crash.slice(0, 200) : "#app 정상 마운트");
  const title = await d.req("GET", `/session/${d.sid}/title`).catch(() => null);
  ok(`${d.tag}: 창 제목`, title === "shareboard", String(title));
}

/// 온보딩 3개 모드의 폼·검증 로직을 훑는다(입력 없이 버튼이 열려 있으면 안 된다).
async function checkOnboarding(d) {
  const modes = await d.texts(".tabs2 button");
  ok(
    `${d.tag}: 온보딩 3모드 표시`,
    modes.length === 3 && modes.join("|") === "이 기기가 서버|참여하기|기존 서버에 만들기",
    modes.join(" / "),
  );

  // 참여하기 — 링크 붙여넣기 + 수동 입력 접기/펼치기
  await d.clickText("참여하기");
  await sleep(300);
  const linkInput = await d.inputValue("shareboard://join");
  ok(`${d.tag}: 참여 모드 링크 입력칸`, linkInput === "", "빈 링크 입력칸 존재");
  ok(`${d.tag}: 링크 없으면 참여 버튼 잠김`, (await d.disabledOfExact("참여")) === true);
  const manualBefore = await d.count("input");
  await d.clickText("링크 없이 수동 입력");
  await sleep(300);
  const manualAfter = await d.count("input");
  ok(
    `${d.tag}: 수동 입력 펼치기`,
    manualAfter === manualBefore + 3,
    `input ${manualBefore} → ${manualAfter} (주소·지문·코드)`,
  );
  await d.shot(`${d.tag}-01-join`);

  // 기존 서버에 만들기 — 필수값 없으면 버튼 잠김
  await d.clickText("기존 서버에 만들기");
  await sleep(300);
  ok(`${d.tag}: 창립 폼 필수값 미입력 시 잠김`, (await d.disabledOf("워크스페이스 만들기")) === true);
  await d.fill("192.168.0.10:45871", "127.0.0.1:45871");
  await d.fill("서버 관리자에게 받은 지문", "ab".repeat(32));
  await d.fill("sb-server --init 출력의 setup 토큰", "TESTTOKEN123");
  await sleep(300);
  ok(`${d.tag}: 필수값 채우면 잠금 해제`, (await d.disabledOf("워크스페이스 만들기")) === false);
  await d.shot(`${d.tag}-02-found`);

  await d.clickText("이 기기가 서버");
  await sleep(300);
}

/// 이 기기를 서버(호스트)로 만든다 → 온보딩이 끝나고 4탭 UI 가 나와야 한다.
async function hostWorkspace(d, name) {
  await d.fill("예) 디자인팀", name);
  const r = await d.clickText("이 기기를 서버로 만들기");
  ok(`${d.tag}: 호스팅 버튼 클릭`, r.ok === true, JSON.stringify(r).slice(0, 200));
  await d.poll(`${d.tag}: 온보딩 종료(탭 UI 등장)`, async () => (await d.count("nav button")) === 4, 30000);
  const tabs = await d.texts("nav button");
  ok(`${d.tag}: 탭 4개`, tabs.length === 4, tabs.join(" / "));
}

async function gotoTab(d, label) {
  const r = await d.clickText(label, "nav button");
  await sleep(500);
  return r;
}

/// 개요 탭 — 호스트 주소/지문/상태 배너/통계.
async function checkOverview(d, { hosting }) {
  await gotoTab(d, "개요");
  // 워커가 내장 서버에 붙고 GK 를 확정할 때까지는 상태가 흔들린다 — 배너로 안정 상태를 기다린다.
  await d
    .poll(
      `${d.tag}: 연결 + 그룹 키 확정`,
      async () => (await d.bodyText()).includes("워크스페이스 준비됨"),
      45000,
    )
    .catch(() => {});
  const body = await d.bodyText();
  if (hosting) {
    ok(`${d.tag}: 개요에 서버 공유 카드`, body.includes("이 기기가 서버입니다"));
    const monos = await d.texts(".card .mono");
    const addr = monos.find((m) => /:\d+$/.test(m));
    ok(`${d.tag}: 호스트 주소 표시`, !!addr && addr.endsWith(":45871"), addr ?? monos.join(","));
    const fpShown = monos.find((m) => /^[0-9a-f]{24}…$/.test(m));
    ok(`${d.tag}: 서버 지문 표시(24자 축약)`, !!fpShown, fpShown ?? monos.join(","));
  }
  ok(`${d.tag}: 헤더 연결 상태 '연결됨'`, (await d.text("header .pill")) === "연결됨", await d.text("header .pill"));
  ok(`${d.tag}: 워크스페이스 준비 배너`, body.includes("워크스페이스 준비됨"), body.includes("그룹 키 대기 중") ? "그룹 키 대기 중" : "");
  const stats = await d.texts(".stat");
  ok(`${d.tag}: 통계 타일 2개(온라인/히스토리)`, stats.length === 2, stats.join(" / "));
  await d.shot(`${d.tag}-03-overview`);
}

/// 멤버 탭 — 목록 + 초대 링크/코드 생성.
async function checkMembers(d, expectCount) {
  await gotoTab(d, "멤버");
  const rows = await d.js(
    `return [...document.querySelectorAll('.card .row')]
        .filter(r => r.querySelector('.dot'))
        .map(r => ({ text: r.innerText.replace(/\\s+/g,' ').trim(), online: !!r.querySelector('.dot.on') }));`,
  );
  ok(`${d.tag}: 멤버 ${expectCount}명 표시`, rows.length === expectCount, rows.map((r) => r.text).join(" | "));
  ok(`${d.tag}: 멤버 온라인 표시`, rows.every((r) => r.online), JSON.stringify(rows.map((r) => r.online)));
  await d.shot(`${d.tag}-04-members`);
  return rows;
}

async function makeInviteLink(d) {
  await d.clickText("초대 링크 생성");
  const link = await d.poll(
    `${d.tag}: 초대 링크 생성`,
    async () => {
      const v = await d.js(
        `const el = [...document.querySelectorAll('input.mono')].find(i => i.readOnly);
         return el ? el.value : null;`,
      );
      return v && v.startsWith("shareboard://join?") ? v : null;
    },
    20000,
  );
  ok(`${d.tag}: 초대 링크 형식`, /^shareboard:\/\/join\?a=[^&]+&f=[0-9a-f]{64}&c=/.test(link), link.slice(0, 80) + "…");
  return link;
}

async function makeInviteCode(d) {
  await d.clickText("코드만 생성");
  const code = await d.poll(`${d.tag}: 초대 코드 생성`, async () => await d.text(".code-box"), 20000);
  ok(`${d.tag}: 초대 코드 형식`, /^[0-9A-Z-]{10,16}$/.test(code), code);
  return code;
}

/// 히스토리 탭 — 실제 X11 클립보드를 건드려 로컬 캡처가 잡히는지 본다.
async function checkHistoryCapture(d, display) {
  await gotoTab(d, "히스토리");
  // 컨테이너를 재사용하면 이전 실행의 클립보드가 남아 있을 수 있다 — 비운 상태에서 시작한다.
  await d.js(`window.confirm = () => true; return true;`);
  await d.clickText("전체 삭제");
  const empty = await d
    .poll(`${d.tag}: 히스토리 비우기`, async () => await d.text(".empty"), 10000)
    .catch(() => null);
  ok(`${d.tag}: 전체 삭제 후 빈 상태`, empty === "아직 항목이 없습니다.", String(empty));

  const marker = `sb-verify-${d.tag}-${Date.now()}`;
  clipSet(display, marker);
  const found = await d.poll(
    `${d.tag}: 클립보드 캡처 → 히스토리 반영`,
    async () => {
      const items = await d.texts(".hist-item");
      return items.find((t) => t.includes(marker)) ?? null;
    },
    20000,
  );
  ok(`${d.tag}: 로컬 클립보드 캡처`, !!found, found?.replace(/\s+/g, " ").slice(0, 90));
  const kinds = await d.texts(".kind-tag");
  ok(`${d.tag}: 종류 태그 text`, kinds[0] === "text", kinds.join(","));
  ok(`${d.tag}: 출처 '내 기기'`, (await d.bodyText()).includes("내 기기"));

  // 핀 토글
  await d.clickText("핀");
  const pinned = await d
    .poll(`${d.tag}: 핀 반영`, async () => (await d.bodyText()).includes("📌"), 10000)
    .catch(() => false);
  ok(`${d.tag}: 핀 토글`, pinned === true);
  await d.shot(`${d.tag}-05-history`);
  return marker;
}

/// 설정 탭 — 플랫폼별 안내, 저장 왕복, 자동 실행 토글.
async function checkSettings(d) {
  await gotoTab(d, "설정");
  const body = await d.bodyText();
  ok(`${d.tag}: 역할이 '서버 호스트'`, body.includes("서버 호스트"));
  ok(
    `${d.tag}: Linux 파일 클립보드 미지원 안내`,
    body.includes("아직 파일 클립보드를 지원하지 않습니다"),
    "files_supported()=false 경로",
  );
  const dir = await d.js(
    `const rows = [...document.querySelectorAll('.row')].find(r => r.innerText.includes('받은 파일 폴더'));
     return rows ? rows.querySelector('.mono').textContent.trim() : null;`,
  );
  ok(`${d.tag}: 받은 파일 폴더 경로 표시`, !!dir && dir.length > 1, String(dir));

  // 자동 실행 토글 — OS 등록까지 가는 경로
  const before = await d.js(
    `const t = [...document.querySelectorAll('[role=switch]')].pop(); return t ? t.getAttribute('aria-checked') : null;`,
  );
  await d.js(
    `const t = [...document.querySelectorAll('[role=switch]')].pop(); if (t) t.click(); return true;`,
  );
  await sleep(1500);
  const after = await d.js(
    `const t = [...document.querySelectorAll('[role=switch]')].pop(); return t ? t.getAttribute('aria-checked') : null;`,
  );
  // 실패했다면 UI 가 배너로 이유를 보여 준다 — 컨테이너 한계인지 앱 문제인지 구분하려면 필요.
  const autoErr = await d.texts(".warn-banner");
  ok(
    `${d.tag}: 자동 실행 토글 반영`,
    before !== after,
    `${before} → ${after}${autoErr.length ? ` · 배너: ${autoErr.join(" | ").slice(0, 200)}` : ""}`,
  );
  await d.shot(`${d.tag}-06-settings`);
}

/// 설정 "저장" 왕복 — 서버 설정을 날려 온보딩으로 되돌아가는 회귀가 있어 **마지막에** 돌린다.
async function checkSettingsSave(d) {
  await gotoTab(d, "설정");
  const name = `검증기기-${d.tag}`;
  await d.fill("비우면 OS 사용자 이름", name);
  await sleep(200);
  await d.clickText("저장");
  await sleep(2000);

  const crash = await d.crashPane();
  const errs = (await d.uiErrors()) ?? [];
  ok(
    `${d.tag}: 설정 저장 시 런타임 오류 없음`,
    crash === null && errs.length === 0,
    `${crash ?? ""} ${JSON.stringify(errs).slice(0, 300)}`,
  );

  // 저장 후에도 워크스페이스 설정이 살아 있어야 한다(탭 UI 유지).
  const navAfter = await d.count("nav button");
  const onboarding = (await d.bodyText()).includes("이 기기를 서버로 만들기");
  ok(
    `${d.tag}: 저장 후에도 워크스페이스 설정 유지`,
    navAfter === 4 && !onboarding,
    navAfter === 4 ? "" : `nav ${navAfter}개 · 온보딩 복귀=${onboarding} (server.addr 유실)`,
  );
  await d.shot(`${d.tag}-08-after-save`);
  if (navAfter !== 4) return;

  await gotoTab(d, "개요");
  await gotoTab(d, "설정");
  const back = await d.inputValue("비우면 OS 사용자 이름");
  ok(`${d.tag}: 설정 저장 왕복`, back === name, `다시 읽음: ${back}`);
}

/// 히스토리 팝업(별도 창) — index.html#quick 이 같은 번들로 렌더된다.
async function checkQuickPanel(d) {
  await gotoTab(d, "설정");
  await d.clickText("지금 열기"); // toggle_quick — 팝업 창을 보여 준다
  await sleep(1500);
  if (!d.quickHandle) {
    ok(`${d.tag}: 팝업 창 핸들`, false, "quick 창 핸들을 못 찾음");
    return;
  }
  await d.switchTo(d.quickHandle);
  const crash = await d.crashPane();
  const ph = await d
    .poll(
      `${d.tag}: 팝업 렌더`,
      async () =>
        await d.js(
          `const i = document.querySelector('.qwrap .qsearch'); return i ? i.placeholder : null;`,
        ),
      15000,
    )
    .catch(() => null);
  ok(
    `${d.tag}: 히스토리 팝업 렌더`,
    crash === null && typeof ph === "string" && ph.includes("히스토리 검색"),
    crash ?? String(ph),
  );
  const qitems = await d.count(".qrow").catch(() => -1);
  ok(`${d.tag}: 팝업이 히스토리 항목 표시`, qitems >= 1, `${qitems}행`);
  await d.shot(`${d.tag}-07-quick`);
  await d.switchTo(d.mainHandle);
}

async function finalErrorCheck(d) {
  const errs = await d.uiErrors().catch(() => []);
  const crash = await d.crashPane().catch(() => null);
  ok(`${d.tag}: 세션 중 JS 오류 없음`, (errs ?? []).length === 0, (errs ?? []).join(" | ").slice(0, 300));
  ok(`${d.tag}: 종료 시점 화면 정상`, crash === null, crash ? crash.slice(0, 200) : "");
}

// ── 시나리오 ─────────────────────────────────────────────────────────

async function scenarioSingle() {
  const d = new Driver(process.env.WD, "A", process.env.DISP);
  await d.start(process.env.APP);
  try {
    await waitBoot(d);
    await d.shot("A-00-onboarding");
    await checkOnboarding(d);
    await hostWorkspace(d, "검증팀");
    await checkOverview(d, { hosting: true });
    await checkMembers(d, 1);
    await makeInviteLink(d);
    await makeInviteCode(d);
    await checkHistoryCapture(d, process.env.DISP);
    await checkSettings(d);
    await checkQuickPanel(d);
    await checkSettingsSave(d); // 파괴적일 수 있어 마지막
    await finalErrorCheck(d);
  } finally {
    await d.quit();
  }
}

async function scenarioTwo() {
  const A = new Driver(process.env.WD_A, "A", process.env.DISP_A);
  const B = new Driver(process.env.WD_B, "B", process.env.DISP_B);
  await A.start(process.env.APP);
  await B.start(process.env.APP);
  try {
    await waitBoot(A);
    await waitBoot(B);

    console.log("\n── A: 워크스페이스 호스팅");
    await hostWorkspace(A, "검증팀");
    await checkOverview(A, { hosting: true });
    await gotoTab(A, "멤버");
    const link = await makeInviteLink(A);

    console.log("\n── B: 초대 링크로 참여");
    await B.clickText("참여하기");
    await sleep(400);
    await B.fill("shareboard://join", link);
    await sleep(300);
    const j = await B.clickTextExact("참여");
    ok("B: 참여 버튼 클릭", j.ok === true, JSON.stringify(j).slice(0, 160));
    await B.poll("B: 온보딩 종료", async () => (await B.count("nav button")) === 4, 45000);
    await B.poll(
      "B: 서버 연결 + 그룹 키 수신",
      async () => (await B.bodyText()).includes("워크스페이스 준비됨"),
      45000,
    );
    ok("B: 워크스페이스 준비 완료", true, "연결 + GK 보유");
    await B.shot("B-03-joined");

    console.log("\n── 멤버 목록 상호 확인");
    await A.poll(
      "A: 멤버 2명 인식",
      async () => (await A.count(".card .row .dot")) >= 2,
      30000,
    );
    await checkMembers(A, 2);
    await checkMembers(B, 2);

    console.log("\n── 클립보드 동기화 A → B");
    await gotoTab(B, "히스토리");
    const m1 = `sb-sync-A2B-${Date.now()}`;
    clipSet(process.env.DISP_A, m1);
    await B.poll(
      "B: A 가 복사한 텍스트 수신",
      async () => (await B.texts(".hist-item")).some((t) => t.includes(m1)),
      30000,
    );
    ok("A→B 히스토리 전달", true, m1);
    const bClip = await (async () => {
      for (let i = 0; i < 30; i++) {
        if (clipGet(process.env.DISP_B) === m1) return m1;
        await sleep(500);
      }
      return clipGet(process.env.DISP_B);
    })();
    ok("A→B OS 클립보드 실제 적용", bClip === m1, `B 클립보드 = ${bClip.slice(0, 60)}`);
    await B.shot("B-05-received");

    console.log("\n── 클립보드 동기화 B → A");
    await gotoTab(A, "히스토리");
    const m2 = `sb-sync-B2A-${Date.now()}`;
    clipSet(process.env.DISP_B, m2);
    await A.poll(
      "A: B 가 복사한 텍스트 수신",
      async () => (await A.texts(".hist-item")).some((t) => t.includes(m2)),
      30000,
    );
    ok("B→A 히스토리 전달", true, m2);
    const aClip = await (async () => {
      for (let i = 0; i < 30; i++) {
        if (clipGet(process.env.DISP_A) === m2) return m2;
        await sleep(500);
      }
      return clipGet(process.env.DISP_A);
    })();
    ok("B→A OS 클립보드 실제 적용", aClip === m2, `A 클립보드 = ${aClip.slice(0, 60)}`);
    await A.shot("A-05-received");

    console.log("\n── 강퇴(그룹 키 회전) 후 접근 상실");
    await gotoTab(A, "멤버");
    await A.js(`window.confirm = () => true; return true;`);
    const rv = await A.clickText("내보내기");
    ok("A: 내보내기 클릭", rv.ok === true, JSON.stringify(rv).slice(0, 200));
    await A.poll(
      "A: 멤버 목록에서 제거",
      async () => (await A.count(".card .row .dot")) === 1,
      45000,
    );
    ok("강퇴 후 A 멤버 1명", true);

    const m3 = `sb-after-revoke-${Date.now()}`;
    clipSet(process.env.DISP_A, m3);
    await sleep(12000);
    const leaked = (await B.texts(".hist-item")).some((t) => t.includes(m3));
    ok("강퇴된 기기는 새 클립을 못 받음", !leaked, leaked ? "누출!" : "12초간 미수신 확인");
    await A.shot("A-08-after-revoke");
    await B.shot("B-08-after-revoke");

    await finalErrorCheck(A);
    await finalErrorCheck(B);
  } finally {
    await A.quit();
    await B.quit();
  }
}

/// 진단용 — 세션이 어떤 창/문서에 붙었는지 그대로 찍어 본다.
async function scenarioProbe() {
  const d = new Driver(process.env.WD, "A", process.env.DISP);
  await d.start(process.env.APP);
  const handles = await d.handles().catch((e) => `handles 실패: ${e.message}`);
  console.log(`handles = ${JSON.stringify(handles)}`);
  for (const h of Array.isArray(handles) ? handles : []) {
    await d.switchTo(h).catch(() => {});
    const info = await d
      .js(
        `return {
          url: location.href, hash: location.hash, title: document.title,
          bodyLen: (document.body && document.body.innerHTML.length) || 0,
          head: (document.body && document.body.innerHTML.slice(0, 300)) || '',
          hasBrand: !!document.querySelector('header .brand'),
          appChildren: (document.getElementById('app') || {}).childElementCount ?? -1,
          tauri: typeof window.__TAURI_INTERNALS__,
        };`,
      )
      .catch((e) => `js 실패: ${e.message}`);
    console.log(`  [${h}] ${JSON.stringify(info)}`);
  }
  await d.quit();
}

// ── main
const mode = process.argv[2] ?? "single";
try {
  if (mode === "probe") await scenarioProbe();
  else if (mode === "single") await scenarioSingle();
  else await scenarioTwo();
} catch (e) {
  fatal = e;
  console.log(`\n💥 시나리오 중단: ${e.message}`);
}

const failed = checks.filter((c) => !c.pass);
console.log(`\n── UI 체크 ${checks.length - failed.length}/${checks.length} 통과`);
for (const f of failed) console.log(`   ❌ ${f.name} — ${f.detail}`);
process.exit(fatal || failed.length ? 1 : 0);
