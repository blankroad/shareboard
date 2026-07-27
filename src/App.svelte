<script lang="ts">
  import { onMount } from "svelte";
  import { api, on, type Status, type Member, type HistoryItem, type AppInfo } from "./lib/ipc";

  let info = $state<AppInfo | null>(null);
  let status = $state<Status | null>(null);
  let members = $state<Member[]>([]);
  let history = $state<HistoryItem[]>([]);
  let tab = $state<"overview" | "members" | "history" | "settings">("overview");

  // 온보딩 — host: 이 기기가 서버 / join: 초대로 참여 / found: 기존 sb-server 에 새로 만들기
  let onbMode = $state<"host" | "join" | "found">("host");
  let fName = $state("");
  let fServer = $state("");
  let fFp = $state("");
  let fCode = $state("");
  let fLink = $state("");
  let fToken = $state("");
  let manualJoin = $state(false);
  let inviteCode = $state<string | null>(null);
  let inviteLink = $state<string | null>(null);
  let busy = $state(false);
  let err = $state<string | null>(null);
  let copied = $state<string | null>(null);

  let settings = $state<any>(null);

  const configured = $derived(!!status?.server_addr);

  async function refresh() {
    try {
      status = await api.getStatus();
      members = await api.getMembers();
      history = await api.getHistory();
    } catch (e) {}
  }

  onMount(async () => {
    info = await api.appInfo();
    settings = await api.getSettings();
    await refresh();
    on("status-changed", () => api.getStatus().then((s) => (status = s)));
    on("members-changed", () => api.getMembers().then((m) => (members = m)));
    on("history-updated", () => api.getHistory().then((h) => (history = h)));
  });

  async function doHost() {
    err = null;
    busy = true;
    try {
      await api.hostWorkspace(fName || "워크스페이스");
      await refresh();
    } catch (e: any) {
      err = String(e);
    } finally {
      busy = false;
    }
  }
  // 기존 sb-server 에 워크스페이스 생성(창립자). 성공/실패는 워커가 status 로 알린다.
  async function doFound() {
    err = null;
    busy = true;
    try {
      await api.createWorkspace(fServer.trim(), fFp.trim(), fName.trim() || "워크스페이스", fToken.trim());
      await refresh();
    } catch (e: any) {
      err = String(e);
    } finally {
      busy = false;
    }
  }
  async function doJoin() {
    err = null;
    busy = true;
    try {
      await api.configureServer(fServer, fFp);
      await api.joinWorkspace(fCode);
      await refresh();
    } catch (e: any) {
      err = String(e);
    } finally {
      busy = false;
    }
  }
  async function doJoinLink() {
    err = null;
    busy = true;
    try {
      await api.joinByLink(fLink.trim());
      await refresh();
    } catch (e: any) {
      err = String(e);
    } finally {
      busy = false;
    }
  }
  async function makeInviteLink() {
    err = null;
    try {
      inviteLink = await api.generateInviteLink();
    } catch (e: any) {
      err = String(e);
    }
  }
  async function toggleSync() {
    if (!status) return;
    await api.setSyncEnabled(!status.sync_enabled);
    status = await api.getStatus();
  }
  async function makeInvite() {
    err = null;
    try {
      inviteCode = await api.generateInvite();
    } catch (e: any) {
      err = String(e);
    }
  }
  async function saveSettings() {
    await api.updateSettings(settings);
    settings = await api.getSettings();
  }
  async function doReset() {
    await api.resetOnboarding();
    await refresh();
  }
  // 창립자만, 연결·GK 있을 때, 자기 자신 제외.
  const canRevoke = (m: Member) =>
    !!status?.is_founder && !!status?.connected && !!status?.gk_present && m.device_id !== status?.device_id;
  async function onRevoke(m: Member) {
    const label = m.name ?? m.addr ?? m.device_id.slice(0, 8);
    if (!confirm(`${label} 님을 내보낼까요?\n그룹 키가 회전되어 이 기기는 더 이상 복호할 수 없게 됩니다. 재참여하려면 새 초대가 필요합니다.`)) return;
    err = null;
    try {
      await api.revokeMember(m.device_id);
      await refresh();
    } catch (e: any) {
      err = String(e);
    }
  }
  async function copy(text: string, label: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = label;
      setTimeout(() => (copied = null), 1200);
    } catch {}
  }

  function fmtTime(ms: number) {
    return new Date(ms).toLocaleTimeString();
  }
</script>

<header>
  <span class="brand">shareboard</span>
  {#if status}
    <span class="dot {status.connected ? 'on' : 'off'}"></span>
    <span class="pill">{status.connected ? "연결됨" : "연결 끊김"}</span>
    {#if status.hosting}<span class="pill">· 이 기기가 서버</span>{/if}
    {#if status.workspace_name}<span class="pill">· {status.workspace_name}</span>{/if}
  {/if}
  <span class="spacer"></span>
  {#if info}<span class="mono">{info.device_id.slice(0, 12)}… · v{info.app_version}</span>{/if}
</header>

{#if !configured}
  <!-- 온보딩 -->
  <main>
    <div class="card" style="max-width:540px;margin:24px auto;">
      <div class="tabs2">
        <button class="btn {onbMode === 'host' ? 'primary' : ''}" onclick={() => (onbMode = "host")}>이 기기가 서버</button>
        <button class="btn {onbMode === 'join' ? 'primary' : ''}" onclick={() => (onbMode = "join")}>참여하기</button>
        <button class="btn {onbMode === 'found' ? 'primary' : ''}" onclick={() => (onbMode = "found")}>기존 서버에 만들기</button>
      </div>
      {#if status?.join_error}<div class="warn-banner">설정 실패: {status.join_error}<br />입력한 값(초대 링크 / 서버 주소·지문 / setup 토큰)을 다시 확인해 주세요.</div>{/if}
      {#if err}<div class="warn-banner">{err}</div>{/if}

      {#if onbMode === "host"}
        <p class="pill">이 컴퓨터가 사내 릴레이 서버가 되고, 워크스페이스를 새로 만듭니다. 다른 사람은 여기 표시되는 주소·지문·초대 코드로 참여합니다. (이 앱은 켜져 있어야 합니다.)</p>
        <label>워크스페이스 이름</label>
        <input bind:value={fName} placeholder="예) 디자인팀" />
        <div class="row"><button class="btn primary grow" disabled={busy} onclick={doHost}>이 기기를 서버로 만들기</button></div>
      {:else if onbMode === "found"}
        <p class="pill">
          이미 사내망에 돌고 있는 <b>sb-server</b>(항상 켜둔 서버)에 워크스페이스를 새로 만듭니다.
          아래 세 값은 서버 관리자가 <span class="mono">sb-server --init</span> 출력에서 알려줍니다.
          <b>워크스페이스당 한 번</b>만 가능하고, 이후 참여자는 "참여하기"로 들어옵니다.
        </p>
        <label>서버 주소 (host:port)</label>
        <input bind:value={fServer} placeholder="192.168.0.10:45871" />
        <label>서버 지문 (64 hex)</label>
        <input class="mono" bind:value={fFp} placeholder="서버 관리자에게 받은 지문" />
        <label>워크스페이스 이름</label>
        <input bind:value={fName} placeholder="예) 디자인팀" />
        <label>setup 토큰 (1회용)</label>
        <input class="mono" bind:value={fToken} placeholder="sb-server --init 출력의 setup 토큰" />
        <div class="row">
          <button class="btn primary grow" disabled={busy || !fServer || !fFp || !fToken} onclick={doFound}>
            워크스페이스 만들기
          </button>
        </div>
      {:else}
        <p class="pill">동료가 보내준 <b>초대 링크</b>를 붙여넣으면 서버 주소·지문·코드가 자동 입력됩니다.</p>
        <label>초대 링크</label>
        <input class="mono" bind:value={fLink} placeholder="shareboard://join?a=…&f=…&c=…" />
        <div class="row"><button class="btn primary grow" disabled={busy || !fLink} onclick={doJoinLink}>참여</button></div>
        <div class="row">
          <button class="btn" style="font-size:12px" onclick={() => (manualJoin = !manualJoin)}>
            {manualJoin ? "▲ 수동 입력 접기" : "▼ 링크 없이 수동 입력"}
          </button>
        </div>
        {#if manualJoin}
          <label>서버 주소 (host:port)</label>
          <input bind:value={fServer} placeholder="192.168.0.10:45871" />
          <label>서버 지문 (64 hex)</label>
          <input class="mono" bind:value={fFp} placeholder="동료에게 받은 지문" />
          <label>초대 코드</label>
          <input class="mono" bind:value={fCode} placeholder="XXXX-XXXX-XXXX" />
          <div class="row"><button class="btn grow" disabled={busy} onclick={doJoin}>수동으로 참여</button></div>
        {/if}
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
      {#if status.hosting && status.host_addr}
        <div class="card">
          <h3>이 기기가 서버입니다 — 다른 사람에게 공유</h3>
          <div class="row">
            <div class="grow">서버 주소</div>
            <span class="mono">{status.host_addr}</span>
            <button class="btn" onclick={() => copy(status!.host_addr!, "주소")}>{copied === "주소" ? "복사됨" : "복사"}</button>
          </div>
          <div class="row">
            <div class="grow">서버 지문</div>
            <span class="mono">{status.host_fingerprint?.slice(0, 24)}…</span>
            <button class="btn" onclick={() => copy(status!.host_fingerprint ?? "", "지문")}>{copied === "지문" ? "복사됨" : "복사"}</button>
          </div>
          <p class="pill">참여할 사람은 위 주소·지문 + <b>멤버 탭에서 발급한 초대 코드</b>를 입력하면 됩니다.</p>
        </div>
      {/if}
      {#if status.joining}
        <div class="warn-banner">처리 중… 서버 연결 · 로그 검증 · 그룹 키 수신을 진행합니다.</div>
      {:else if status.connected && status.gk_present}
        <div class="ok-banner">✅ 워크스페이스 준비됨 · 동기화 중</div>
      {:else if !status.gk_present}
        <div class="warn-banner">그룹 키 대기 중 — 초대한 멤버가 온라인이 되면 자동으로 완료됩니다.</div>
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
              <div class="grow">
                <span style="font-weight:600">
                  {m.name ?? (m.device_id === status?.device_id ? "이 기기" : (m.addr ?? "주소 미상"))}
                </span>
                {#if m.addr && m.name}<span class="pill" style="margin-left:6px">{m.addr}</span>{/if}
                <span class="mono" style="margin-left:6px;font-size:11px;color:var(--muted)">{m.device_id.slice(0, 8)}</span>
              </div>
              <span class="pill">{m.online ? "온라인" : "오프라인"}</span>
              {#if canRevoke(m)}
                <button class="btn danger" onclick={() => onRevoke(m)} title="이 멤버를 내보내고 그룹 키를 회전합니다">내보내기</button>
              {/if}
            </div>
          {/each}
        {/if}
      </div>
      <div class="card">
        <h3>초대</h3>
        <div class="row">
          <button class="btn primary" onclick={makeInviteLink}>초대 링크 생성</button>
          <button class="btn" onclick={makeInvite}>코드만 생성</button>
        </div>
        {#if inviteLink}
          <label>초대 링크 (이거 하나만 전달하면 됩니다)</label>
          <div class="row">
            <input class="mono grow" readonly value={inviteLink} />
            <button class="btn primary" onclick={() => copy(inviteLink ?? "", "링크")}>{copied === "링크" ? "복사됨" : "링크 복사"}</button>
          </div>
          <p class="pill">사내 메신저로 전달 → 받은 사람은 "참여하기"에 붙여넣기. (1시간·1회용, 코드가 들어있으니 취급 주의)</p>
        {/if}
        {#if inviteCode}
          <div class="code-box">{inviteCode}</div>
          <div class="row">
            <span class="grow pill">서버 주소·지문과 함께 전달해야 합니다 (1시간, 1회용).</span>
            <button class="btn" onclick={() => copy(inviteCode ?? "", "코드")}>{copied === "코드" ? "복사됨" : "코드 복사"}</button>
          </div>
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
        <h3>연결 / 역할</h3>
        {#if status?.hosting}
          <div class="row"><div class="grow">역할</div><span class="pill" style="background:var(--primary-weak);color:var(--primary)">서버 호스트 (이 기기가 서버)</span></div>
          <div class="row"><div class="grow">서버 주소</div><span class="mono">{status.host_addr}</span><button class="btn" onclick={() => copy(status?.host_addr ?? "", "주소")}>{copied === "주소" ? "복사됨" : "복사"}</button></div>
          <div class="row"><div class="grow">서버 지문</div><span class="mono">{status.host_fingerprint?.slice(0, 20)}…</span><button class="btn" onclick={() => copy(status?.host_fingerprint ?? "", "지문")}>{copied === "지문" ? "복사됨" : "복사"}</button></div>
        {:else}
          <div class="row"><div class="grow">역할</div><span class="pill">클라이언트</span></div>
          <div class="row"><div class="grow">접속 서버</div><span class="mono">{status?.server_addr ?? "-"}</span></div>
        {/if}
        <div class="row">
          <span class="grow pill">역할을 바꾸려면(서버↔클라이언트) 재설정 후 처음 화면에서 다시 선택합니다.</span>
          <button class="btn danger" onclick={doReset}>서버/역할 다시 설정</button>
        </div>
      </div>
      <div class="card">
        <h3>기기 이름</h3>
        <p class="pill">다른 멤버에게 IP 대신 이 이름으로 보입니다(집처럼 IP가 바뀌어도 유지). 비우면 OS 사용자 이름을 사용합니다.</p>
        <input bind:value={settings.app.device_name_override} placeholder="비우면 OS 사용자 이름 (예: chulsoo)" />
      </div>
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
