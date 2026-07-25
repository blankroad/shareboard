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
  hosting: boolean;
  host_addr: string | null;
  host_fingerprint: string | null;
};

export type HostInfo = { addr: string; fingerprint_hex: string };

export type Member = { device_id: string; name: string; online: boolean; platform: string };

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

export type AppInfo = { app_version: string; proto_min: number; proto_max: number; device_id: string };

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  getStatus: () => invoke<Status>("get_status"),
  getMembers: () => invoke<Member[]>("get_members"),
  getSettings: () => invoke<any>("get_settings"),
  updateSettings: (settings: any) => invoke("update_settings", { settings }),
  setSyncEnabled: (enabled: boolean) => invoke("set_sync_enabled", { enabled }),
  getHistory: () => invoke<HistoryItem[]>("get_history"),
  copyHistoryItem: (id: string) => invoke<boolean>("copy_history_item", { id }),
  deleteHistoryItem: (id: string) => invoke("delete_history_item", { id }),
  setPinned: (id: string, pinned: boolean) => invoke("set_pinned", { id, pinned }),
  clearHistory: () => invoke("clear_history"),
  configureServer: (addr: string, fingerprint_hex: string, workspace_name?: string) =>
    invoke("configure_server", { addr, fingerprint_hex, workspace_name }),
  createWorkspace: (name: string, setup_token: string) =>
    invoke("create_workspace", { name, setup_token, now_ms: Date.now() }),
  joinWorkspace: (code: string) => invoke("join_workspace", { code }),
  generateInvite: () => invoke<string>("generate_invite", { expires_at: Date.now() + 3600_000 }),
  generateInviteLink: () => invoke<string>("generate_invite_link"),
  joinByLink: (link: string) => invoke("join_by_link", { link }),
  hostWorkspace: (name: string) => invoke<HostInfo>("host_workspace", { name }),
  getHostInfo: () => invoke<HostInfo | null>("get_host_info"),
};

export async function on(event: string, cb: () => void): Promise<UnlistenFn> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen(event, () => cb());
}
