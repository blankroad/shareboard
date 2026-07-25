import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

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
  mount(App, { target: document.getElementById("app")! });
} catch (e) {
  showError("mount-failed", e);
}
