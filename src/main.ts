import { mount } from "svelte";
import App from "./App.svelte";
import QuickPanel from "./lib/QuickPanel.svelte";
import "./app.css";

// 같은 번들을 두 창이 공유한다 — "quick" 창은 index.html#quick 로 열려 팝업만 렌더한다.
const isQuick = location.hash === "#quick";

// 블랭크 화면 진단: 어떤 런타임 오류든 화면에 보이게 만든다.
function showError(label: string, detail: unknown) {
  const el = document.getElementById("app");
  const msg =
    detail instanceof Error ? `${detail.message}\n${detail.stack ?? ""}` : String(detail);
  if (el) {
    el.innerHTML = `<pre style="color:#e11;background:#fff;padding:16px;white-space:pre-wrap;font:12px monospace">[${label}]\n${msg}</pre>`;
  }
}

window.addEventListener("error", (e) => showError("window.error", e.error ?? e.message));
window.addEventListener("unhandledrejection", (e) => showError("unhandledrejection", e.reason));

try {
  mount(isQuick ? QuickPanel : App, { target: document.getElementById("app")! });
} catch (e) {
  showError("mount-failed", e);
}
