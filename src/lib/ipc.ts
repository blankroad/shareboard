// Rust 커맨드/이벤트 래퍼. Tauri API 는 **지연 로딩**한다 — 최상위 import 가 실패해도
// UI 셸은 렌더되도록(런타임에 __TAURI_INTERNALS__ 부재 등으로 죽지 않게).
import type { UnlistenFn } from "@tauri-apps/api/event";

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export type Status = {
  connected: boolean;
  server_addr: string | null;
  workspace_name: string | null;
  member_count: number;
  online_count: number;
  sync_enabled: boolean;
  gk_present: boolean;
  device_id: string;
  is_founder: boolean;
  hosting: boolean;
  host_addr: string | null;
  host_fingerprint: string | null;
  joining: boolean;
  join_error: string | null;
};

export type HostInfo = { addr: string; fingerprint_hex: string };

export type Member = {
  device_id: string;
  name: string;
  online: boolean;
  platform: string;
  addr: string | null;
};

export type HistoryItem = {
  id: string;
  kind: string;
  origin: string;
  size: number;
  created_at: number;
  preview: string;
  pinned: boolean;
  has_body: boolean;
};

/// 이미지 썸네일 — data URL + **원본** 픽셀 크기.
export type Thumb = { data_url: string; width: number; height: number };

export type AppInfo = { app_version: string; proto_min: number; proto_max: number; device_id: string };

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  getStatus: () => invoke<Status>("get_status"),
  getMembers: () => invoke<Member[]>("get_members"),
  revokeMember: (device_id: string) => invoke("revoke_member", { device_id }),
  getSettings: () => invoke<any>("get_settings"),
  updateSettings: (settings: any) => invoke("update_settings", { settings }),
  setSyncEnabled: (enabled: boolean) => invoke("set_sync_enabled", { enabled }),
  getHistory: () => invoke<HistoryItem[]>("get_history"),
  getThumbnail: (id: string) => invoke<Thumb | null>("get_thumbnail", { id }),
  copyHistoryItem: (id: string) => invoke<boolean>("copy_history_item", { id }),
  deleteHistoryItem: (id: string) => invoke("delete_history_item", { id }),
  setPinned: (id: string, pinned: boolean) => invoke("set_pinned", { id, pinned }),
  clearHistory: () => invoke("clear_history"),
  configureServer: (addr: string, fingerprint_hex: string, workspace_name?: string) =>
    invoke("configure_server", { addr, fingerprint_hex, workspace_name }),
  // 기존 sb-server 에 워크스페이스를 새로 만든다(창립자). 주소·지문 설정까지 한 번에 처리.
  createWorkspace: (addr: string, fingerprint_hex: string, name: string, setup_token: string) =>
    invoke("create_workspace", { addr, fingerprint_hex, name, setup_token, now_ms: Date.now() }),
  joinWorkspace: (code: string) => invoke("join_workspace", { code }),
  generateInvite: () => invoke<string>("generate_invite", { expires_at: Date.now() + 3600_000 }),
  generateInviteLink: () => invoke<string>("generate_invite_link"),
  joinByLink: (link: string) => invoke("join_by_link", { link }),
  resetOnboarding: () => invoke("reset_onboarding"),
  hostWorkspace: (name: string) => invoke<HostInfo>("host_workspace", { name }),
  getHostInfo: () => invoke<HostInfo | null>("get_host_info"),
  toggleQuick: () => invoke("toggle_quick"),
  getReceivedDir: () => invoke<string>("get_received_dir"),
  openReceivedDir: () => invoke("open_received_dir"),
  filesSupported: () => invoke<boolean>("files_supported"),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) => invoke("set_autostart", { enabled }),
  setQuickHotkey: (accel: string) => invoke("set_quick_hotkey", { accel }),
};

/// 자기 창 숨기기 — 팝업이 Esc/복사 후 사라질 때 사용.
export async function hideSelf() {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().hide();
}

/// 썸네일 캐시 키 — 본문 도착 여부가 바뀌면 다시 시도하도록 섞는다.
export const thumbKey = (h: HistoryItem) => `${h.id}:${h.has_body ? 1 : 0}`;

/// 이미지 항목 썸네일을 지연 로드해 `into` 에 채운다(이미 시도한 키는 건너뜀).
export async function loadThumbs(list: HistoryItem[], into: Record<string, Thumb | null>) {
  for (const h of list) {
    if (h.kind !== "image") continue;
    const k = thumbKey(h);
    if (k in into) continue;
    into[k] = null; // 중복 요청 방지
    try {
      into[k] = await api.getThumbnail(h.id);
    } catch {}
  }
  // 사라진 항목의 썸네일 정리.
  const alive = new Set(list.filter((h) => h.kind === "image").map(thumbKey));
  for (const k of Object.keys(into)) if (!alive.has(k)) delete into[k];
}

export async function on(event: string, cb: () => void): Promise<UnlistenFn> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen(event, () => cb());
}
