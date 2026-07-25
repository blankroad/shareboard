<script lang="ts">
  import { onMount } from "svelte";
  import { api, on, type Status, type Member, type HistoryItem, type AppInfo } from "./lib/ipc";

  let info = $state<AppInfo | null>(null);
  let status = $state<Status | null>(null);
  let members = $state<Member[]>([]);
  let history = $state<HistoryItem[]>([]);
  let tab = $state<"overview" | "members" | "history" | "settings">("overview");

  // 온보딩 폼
  let onbMode = $state<"create" | "join">("create");
  let fServer = $state("");
  let fFp = $state("");
  let fName = $state("");
  let fToken = $state("");
  let fCode = $state("");
  let inviteCode = $state<string | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);

  let settings = $state<any>(null);

  const configured = $derived(!!status?.server_addr);

  async function refresh() {
    try {
      status = await api.getStatus();
      members = await api.getMembers();
      history = await api.getHistory();
    } catch (e) { /* 초기 로딩 중 */ }
  }

  onMount(async () => {
    info = await api.appInfo();
    settings = await api.getSettings();
    await refresh();
    on("status-changed", () => api.getStatus().then((s) => (status = s)));
    on("members-changed", () => api.getMembers().then((m) => (members = m)));
    on("history-updated", () => api.getHistory().then((h) => (history = h)));
  });

  async function doCreate() {
    err = null; busy = true;
    try {
      await api.configureServer(fServer, fFp, fName);
      await api.createWorkspace(fName, fToken);
      await refresh();
    } catch (e: any) { err = String(e); } finally { busy = false; }
  }
  async function doJoin() {
    err = null; busy = true;
    try {
      await api.configureServer(fServer, fFp);
      await api.joinWorkspace(fCode);
      await refresh();
    } catch (e: any) { err = String(e); } finally { busy = false; }
  }
  async function toggleSync() {
    if (!status) return;
    await api.setSyncEnabled(!status.sync_enabled);
    status = await api.getStatus();
  }
  async function makeInvite() {
    err = null;
    try { inviteCode = await api.generateInvite(); }
    catch (e: any) { err = String(e); }
  }
  async function saveSettings() {
    await api.updateSettings(settings);
    settings = await api.getSettings();
  }

  function fmtTime(ms: number) { return new Date(ms).toLocaleTimeString(); }
  function fmtSize(n: number) { return n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`; }
</script>

<header>
  <span class="brand">shareboard</span>
  {#if status}
    <span class="dot {status.connected ? 'on' : 'off'}"></span>
    <span class="pill">{status.connected ? "연결됨" : "연결 끊김"}</span>
    {#if status.workspace_name}<span class="pill">· {status.workspace_name}</span>{/if}
  {/if}
  <span class="spacer"></span>
  {#if info}<span class="mono">{info.device_id.slice(0, 12)}… · v{info.app_version}</span>{/if}
</header>

{#if !configured}
  <!-- 온보딩 -->
  <main>
    <div class="card" style="max-width:520px;margin:24px auto;">
      <div class="tabs2">
        <button class="btn {onbMode === 'create' ? 'primary' : ''}" onclick={() => (onbMode = "create")}>워크스페이스 만들기</button>
        <button class="btn {onbMode === 'join' ? 'primary' : ''}" onclick={() => (onbMode = "join")}>참여하기</button>
      </div>
      {#if err}<div class="warn-banner">{err}</div>{/if}

      <label>서버 주소 (host:port)</label>
      <input bind:value={fServer} placeholder="192.168.1.10:45871" />
      <label>서버 지문 (SHA-256 SPKI, 64 hex)</label>
      <input class="mono" bind:value={fFp} placeholder="서버 관리자에게 받은 지문" />

      {#if onbMode === "create"}
        <label>워크스페이스 이름</label>
        <input bind:value={fName} placeholder="개발팀" />
        <label>Setup 토큰 (관리자 발급)</label>
        <input bind:value={fToken} placeholder="sb-server --gen-token 으로 생성" />
        <div class="row"><button class="btn primary grow" disabled={busy} onclick={doCreate}>만들기</button></div>
      {:else}
        <label>초대 코드</label>
        <input class="mono" bind:value={fCode} placeholder="XXXX-XXXX-XXXX" />
        <div class="row"><button class="btn primary grow" disabled={busy} onclick={doJoin}>참여</button></div>
      {/if}
    </div>
  </main>
{:else}
  <nav>
    <button class:active={tab === "overview"} onclick={() => (tab = "overview")}>개요</button>
    <button class:active={tab === "members"} onclick={() => (tab = "members")}>멤버 {members.length ? `(${members.length})` : ""}</button>
    <button class:active={tab === "history"} onclick={() => (tab = "history")}>히스토리</button>
    <button class:active={tab === "settings"} onclick={() => (tab = "settings")}>설정</button>
  </nav>

  <main>
    {#if tab === "overview" && status}
      {#if !status.gk_present}
        <div class="warn-banner">그룹 키 대기 중 — 멤버가 온라인이 되면 자동으로 동기화가 시작됩니다.</div>
      {/if}
      <div class="card">
        <div class="grid2">
          <div><div class="stat">{status.online_count}/{status.member_count}</div><div class="stat-label">온라인 / 전체 멤버</div></div>
          <div><div class="stat">{history.length}</div><div class="stat-label">히스토리 항목</div></div>
        </div>
      </div>
      <div class="card">
        <h3>동기화</h3>
        <div class="row">
          <div class="grow">클립보드 동기화</div>
          <div class="toggle" onclick={toggleSync} role="switch" tabindex="0" aria-checked={status.sync_enabled}>
            <div class="switch {status.sync_enabled ? 'on' : ''}"></div>
          </div>
        </div>
        <div class="row"><div class="grow">서버</div><span class="mono">{status.server_addr}</span></div>
      </div>
    {/if}

    {#if tab === "members"}
      <div class="card">
        <h3>멤버</h3>
        {#if members.length === 0}
          <div class="empty">멤버 정보를 불러오는 중…</div>
        {:else}
          {#each members as m}
            <div class="row">
              <span class="dot {m.online ? 'on' : 'off'}"></span>
              <div class="grow"><span class="mono">{m.device_id.slice(0, 16)}…</span></div>
              <span class="pill">{m.online ? "온라인" : "오프라인"}</span>
            </div>
          {/each}
        {/if}
      </div>
      <div class="card">
        <h3>초대</h3>
        <div class="row"><button class="btn primary" onclick={makeInvite}>초대 코드 생성</button></div>
        {#if inviteCode}
          <div class="code-box">{inviteCode}</div>
          <p class="pill" style="text-align:center">이 코드를 사내 메신저로 전달하세요 (1시간 유효, 1회용).</p>
        {/if}
        {#if err}<div class="warn-banner">{err}</div>{/if}
      </div>
    {/if}

    {#if tab === "history"}
      <div class="card">
        <div class="row"><h3 class="grow" style="margin:0">히스토리</h3><button class="btn danger" onclick={() => api.clearHistory()}>전체 삭제</button></div>
        {#if history.length === 0}
          <div class="empty">아직 항목이 없습니다.</div>
        {:else}
          {#each history as h}
            <div class="hist-item">
              <span class="kind-tag">{h.kind}</span>
              <span class="preview">{h.preview || "(이미지)"}</span>
              <span class="pill">{h.origin === "local" ? "내 기기" : h.origin.slice(0, 8)}</span>
              <span class="pill">{fmtTime(h.created_at)}</span>
              <button class="btn" disabled={!h.has_body} onclick={() => api.copyHistoryItem(h.id)}>복사</button>
              <button class="btn" onclick={() => api.setPinned(h.id, !h.pinned)}>{h.pinned ? "📌" : "핀"}</button>
              <button class="btn danger" onclick={() => api.deleteHistoryItem(h.id)}>✕</button>
            </div>
          {/each}
        {/if}
      </div>
    {/if}

    {#if tab === "settings" && settings}
      <div class="card">
        <h3>동기화</h3>
        <label class="toggle"><input type="checkbox" bind:checked={settings.sync.sync_text} style="width:auto" /> 텍스트 동기화</label>
        <label class="toggle"><input type="checkbox" bind:checked={settings.sync.sync_images} style="width:auto" /> 이미지 동기화</label>
        <label class="toggle"><input type="checkbox" bind:checked={settings.sync.confirm_risky_content} style="width:auto" /> 고위험 콘텐츠 확인 후 적용</label>
      </div>
      <div class="card">
        <h3>히스토리 / 개인정보</h3>
        <label class="toggle"><input type="checkbox" bind:checked={settings.history.persist_enabled} style="width:auto" /> 디스크에 암호화 저장(기본 꺼짐)</label>
        <label class="toggle"><input type="checkbox" bind:checked={settings.privacy.exclude_concealed} style="width:auto" /> 비밀번호 매니저 콘텐츠 제외</label>
        <label>인메모리 히스토리 최대 개수</label>
        <input type="number" bind:value={settings.history.memory_max_items} />
      </div>
      <div class="row"><button class="btn primary" onclick={saveSettings}>저장</button></div>
    {/if}
  </main>
{/if}
