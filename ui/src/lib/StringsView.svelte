<script lang="ts">
  // Readable text runs found in a file's bytes, with their offsets; an offset opens the bytes
  // there. The list is filtered in the window and only the rows in sight are drawn.
  import { fileStrings } from "./api";

  let { path, size, onoffset }: { path: string; size: number; onoffset: (offset: number) => void } = $props();

  const ROW = 22;

  let all = $state.raw<[number, string][] | null>(null);
  let scanned = $state(0);
  let cut = $state(false);
  let failure = $state<string | null>(null);
  let filter = $state("");
  let scrollTop = $state(0);
  let height = $state(400);
  let scroller: HTMLDivElement;

  $effect(() => {
    const p = path;
    all = null;
    failure = null;
    fileStrings(p)
      .then((r) => {
        if (p !== path) return;
        all = r.strings;
        scanned = r.scanned;
        cut = r.cut;
      })
      .catch((e) => (failure = String(e)));
  });

  const shown = $derived.by(() => {
    const needle = filter.trim().toLowerCase();
    if (!all || !needle) return all ?? [];
    return all.filter(([, s]) => s.toLowerCase().includes(needle));
  });
  const digits = $derived(Math.max(8, (all?.at(-1)?.[0] ?? 0).toString(16).length));
  const first = $derived(Math.floor(scrollTop / ROW));
  const rows = $derived(shown.slice(first, first + Math.ceil(height / ROW) + 1));

  $effect(() => {
    void filter;
    if (scroller) scroller.scrollTop = 0;
  });
</script>

<div class="strings">
  <div class="bar">
    <input bind:value={filter} placeholder="Filter" aria-label="Filter strings" spellcheck="false" />
    {#if all}
      <span class="muted">
        {shown.length.toLocaleString()}{filter.trim() ? ` of ${all.length.toLocaleString()}` : ""}
        {all.length === 1 ? "string" : "strings"}{scanned < size ? ` in the first ${Math.round(scanned / 1048576)} MiB` : ""}{cut
          ? "; the list stops at the display limit"
          : ""}
      </span>
    {:else if !failure}
      <span class="muted">Reading…</span>
    {/if}
  </div>
  {#if failure}<p class="error">{failure}</p>{/if}
  <div
    class="scroller"
    bind:this={scroller}
    bind:clientHeight={height}
    onscroll={() => (scrollTop = scroller.scrollTop)}
    role="list"
    aria-label="Strings found in the file"
  >
    <div style:height="{shown.length * ROW}px" class="space">
      <div style:transform="translateY({first * ROW}px)">
        {#each rows as [offset, text], i (first + i)}
          <div class="row" role="listitem">
            <button class="offset" title="Show the bytes at this offset" onclick={() => onoffset(offset)}>
              {offset.toString(16).toUpperCase().padStart(digits, "0")}
            </button>
            <span class="text">{text}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .strings {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
    font-size: 12.5px;
  }
  .bar input {
    font: inherit;
    width: 240px;
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .scroller {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .space {
    position: relative;
  }
  .row {
    display: flex;
    gap: 16px;
    height: 22px;
    align-items: center;
    padding: 0 12px;
    font-family: var(--mono);
    font-size: 12.5px;
    white-space: pre;
  }
  .offset {
    font: inherit;
    border: none;
    background: none;
    padding: 0;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .offset:hover,
  .offset:focus-visible {
    color: var(--accent);
    text-decoration: underline;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .muted {
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
    padding: 12px;
  }
</style>
