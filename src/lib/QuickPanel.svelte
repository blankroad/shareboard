<script lang="ts">
  // 히스토리 팝업(label "quick"). 글로벌 핫키로 열리고, 고르면 클립보드에 복사한 뒤 사라진다.
  // 붙여넣기 자체는 사용자가 한다(자동 붙여넣기는 macOS 접근성 권한이 필요해 v1 범위 밖).
  import { onMount } from "svelte";
  import { api, on, hideSelf, loadThumbs, thumbKey, type HistoryItem, type Thumb } from "./ipc";

  let items = $state<HistoryItem[]>([]);
  let thumbs = $state<Record<string, Thumb | null>>({});
  let q = $state("");
  let sel = $state(0);
  let box = $state<HTMLInputElement | null>(null);

  const norm = (s: string) => s.toLowerCase();
  const filtered = $derived(
    q.trim() ? items.filter((h) => norm(h.preview).includes(norm(q.trim()))) : items,
  );
  // 필터 결과가 줄어들면 선택이 범위를 벗어날 수 있다.
  const cursor = $derived(Math.min(sel, Math.max(filtered.length - 1, 0)));

  async function reload() {
    try {
      items = await api.getHistory();
      void loadThumbs(items, thumbs);
    } catch {}
  }

  onMount(async () => {
    await reload();
    box?.focus();
    on("history-updated", () => void reload());
    // 핫키로 다시 열릴 때마다 초기 상태로.
    on("quick-opened", () => {
      q = "";
      sel = 0;
      void reload();
      box?.focus();
      box?.select();
    });
  });

  // 선택 행이 화면에서 벗어나지 않도록.
  $effect(() => {
    cursor;
    filtered.length;
    document.querySelector(".qrow.sel")?.scrollIntoView({ block: "nearest" });
  });

  async function pick(h: HistoryItem | undefined) {
    if (!h || !h.has_body) return;
    try {
      await api.copyHistoryItem(h.id);
    } catch {}
    await hideSelf();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      void hideSelf();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      sel = Math.min(cursor + 1, filtered.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      sel = Math.max(cursor - 1, 0);
    } else if (e.key === "Enter") {
      e.preventDefault();
      void pick(filtered[cursor]);
    }
  }

  function fmtSize(n: number) {
    if (n >= 1024 * 1024) return `${(n / 1048576).toFixed(1)} MB`;
    if (n >= 1024) return `${Math.round(n / 1024)} KB`;
    return `${n} B`;
  }
  function fmtTime(ms: number) {
    return new Date(ms).toLocaleTimeString();
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="qwrap">
  <input
    class="qsearch"
    bind:this={box}
    bind:value={q}
    placeholder="히스토리 검색 — ↑↓ 이동, Enter 복사, Esc 닫기"
  />

  {#if filtered.length === 0}
    <div class="empty">{items.length === 0 ? "아직 항목이 없습니다." : "검색 결과가 없습니다."}</div>
  {:else}
    <div class="qlist">
      {#each filtered as h, i (h.id)}
        <button
          class="qrow {i === cursor ? 'sel' : ''}"
          disabled={!h.has_body}
          onclick={() => pick(h)}
          onmouseenter={() => (sel = i)}
        >
          {#if h.kind === "image"}
            {@const t = thumbs[thumbKey(h)]}
            {#if t}
              <img class="qthumb" src={t.data_url} alt="" />
              <span class="preview">이미지 · {t.width}×{t.height} · {fmtSize(h.size)}</span>
            {:else}
              <span class="qthumb qthumb-empty" aria-hidden="true">🖼</span>
              <span class="preview">이미지 · {fmtSize(h.size)}</span>
            {/if}
          {:else if h.kind === "files"}
            <span class="qthumb qthumb-empty" aria-hidden="true">📄</span>
            <span class="preview">{h.preview} · {fmtSize(h.size)}</span>
          {:else}
            <span class="qthumb qthumb-empty" aria-hidden="true">T</span>
            <span class="preview">{h.preview}</span>
          {/if}
          <span class="pill">{h.origin === "local" ? "내 기기" : h.origin.slice(0, 8)}</span>
          <span class="pill">{fmtTime(h.created_at)}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>
