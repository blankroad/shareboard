// Tauri 커맨드 인자 이름 대조 — Rust 시그니처 ↔ 프런트 invoke payload.
//
// Tauri v2 는 payload 키를 **camelCase** 로 기대한다(Rust `device_id` → JS `deviceId`).
// snake_case 로 보내면 컴파일·타입체크는 통과하고 **런타임에만**
// "invalid args `deviceId` for command `revoke_member`" 로 터진다 — 실제로 내보내기·초대
// 코드 생성·기존 서버에 워크스페이스 만들기가 이 이유로 조용히 깨져 있었다.
//
//   node scripts/check-ipc-args.mjs
//
// 종료 코드 0 = 일치. 1 = 불일치(어느 커맨드가 무엇을 기대하는지 출력).

import { readFileSync } from "node:fs";

const RUST = "src-tauri/src/commands.rs";
const TS = "src/lib/ipc.ts";

const camel = (s) =>
  s.split("_").reduce((acc, w, i) => acc + (i === 0 ? w : w[0].toUpperCase() + w.slice(1)), "");

/** Rust 커맨드 → 인자 이름 목록(AppHandle/State 는 IPC 인자가 아니라 제외). */
function rustCommands(src) {
  const out = new Map();
  const re = /#\[tauri::command[^\]]*\]\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([^)]*)\)/gs;
  for (const m of src.matchAll(re)) {
    const args = m[2]
      .split(",")
      .map((p) => p.trim())
      .filter((p) => p.includes(":"))
      .map((p) => [p.slice(0, p.indexOf(":")).trim(), p.slice(p.indexOf(":") + 1).trim()])
      .filter(([, ty]) => !ty.includes("AppHandle") && !ty.includes("State<"))
      .map(([name]) => name);
    out.set(m[1], args);
  }
  return out;
}

/** 프런트 invoke("cmd", { … }) → payload 키 목록. */
function tsCalls(src) {
  const calls = [];
  const re = /invoke(?:<[^>]*>)?\(\s*"(\w+)"\s*(?:,\s*\{([^}]*)\})?\s*\)/gs;
  for (const m of src.matchAll(re)) {
    const keys = (m[2] ?? "")
      .split(",")
      .map((k) => k.trim())
      .filter(Boolean)
      .map((k) => (k.includes(":") ? k.slice(0, k.indexOf(":")).trim() : k));
    calls.push({ cmd: m[1], keys });
  }
  return calls;
}

const commands = rustCommands(readFileSync(RUST, "utf8"));
const problems = [];

for (const { cmd, keys } of tsCalls(readFileSync(TS, "utf8"))) {
  if (!commands.has(cmd)) {
    problems.push(`${cmd}: Rust 에 그런 커맨드가 없다(오타이거나 등록 누락)`);
    continue;
  }
  const expected = commands.get(cmd).map(camel);
  const got = [...keys].sort();
  if (JSON.stringify(got) !== JSON.stringify([...expected].sort())) {
    problems.push(`${cmd}: 보내는 키 [${keys}] ↔ 기대하는 키 [${expected}]`);
  }
}

if (problems.length > 0) {
  console.error("IPC 인자 이름 불일치 — Tauri v2 는 camelCase 를 기대한다:\n");
  for (const p of problems) console.error(`  ✗ ${p}`);
  console.error(`\n대상: ${TS} / 정의: ${RUST}`);
  process.exit(1);
}
console.log(`IPC 인자 이름 OK (커맨드 ${commands.size}개 대조)`);
