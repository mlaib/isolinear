<script lang="ts">
  // Read-only view of a text file of any size. Lines come from the Rust index a page at a
  // time; only the visible rows are in the DOM. The scrollbar is a proxy: its position maps
  // to a line number, so files with more lines than a browser can lay out still scroll.
  import { onMount, tick } from "svelte";
  import {
    pagedLines,
    pagedFind,
    onIndexProgress,
    LINE_CAP,
    type Found,
    type IndexProgress,
    type PagedLine,
  } from "./api";

  let { id, onprogress }: { id: number; onprogress?: (p: IndexProgress & { error: string | null }) => void } =
    $props();

  const ROW = 20; // px; matches .row line-height
  const PAGE = 256;
  const KEEP_PAGES = 48;
  const MAX_SPACER = 2_000_000; // px, well under every engine's element height limit

  let available = $state(0);
  let complete = $state(false);
  let first = $state(0);
  let rows = $state(1);
  let version = $state(0); // bumps when pages arrive or are dropped
  const pages = new Map<number, PagedLine[]>();
  const loading = new Set<number>();
  let failure: string | null = $state(null);

  let root: HTMLDivElement;
  let content: HTMLDivElement;
  let bar: HTMLDivElement;
  let expectedTop = -1;

  let findOpen = $state(false);
  let query = $state("");
  let matchCase = $state(false);
  let searching = $state(false);
  let findStatus = $state("");
  let match: Found | null = $state(null);
  let findInput: HTMLInputElement | undefined = $state();

  let gotoOpen = $state(false);
  let gotoValue = $state("");
  let gotoInput: HTMLInputElement | undefined = $state();
  let current: number | null = $state(null);

  const maxFirst = $derived(Math.max(0, available - rows));
  const spacer = $derived(Math.min(available * ROW, MAX_SPACER));
  const digits = $derived(String(Math.max(available, 1)).length);

  const visible = $derived.by(() => {
    void version;
    const out: { n: number; line: PagedLine | null }[] = [];
    const end = Math.min(available, first + rows + 1);
    for (let n = first; n < end; n++) {
      out.push({ n, line: pages.get(Math.floor(n / PAGE))?.[n % PAGE] ?? null });
    }
    return out;
  });

  function setFirst(n: number) {
    first = Math.max(0, Math.min(maxFirst, Math.round(n)));
    syncBar();
  }

  function syncBar() {
    if (!bar) return;
    const range = bar.scrollHeight - bar.clientHeight;
    const top = maxFirst > 0 && range > 0 ? (first / maxFirst) * range : 0;
    expectedTop = Math.round(top);
    bar.scrollTop = top;
  }

  function onBarScroll() {
    if (Math.abs(bar.scrollTop - expectedTop) <= 1) return;
    expectedTop = -1;
    const range = bar.scrollHeight - bar.clientHeight;
    first = range > 0 ? Math.round((bar.scrollTop / range) * maxFirst) : 0;
  }

  let wheelRest = 0;
  function onWheel(e: WheelEvent) {
    if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return; // horizontal: the content scrolls
    e.preventDefault();
    const lines = e.deltaMode === 1 ? e.deltaY : e.deltaMode === 2 ? e.deltaY * rows : e.deltaY / ROW;
    wheelRest += lines;
    const whole = Math.trunc(wheelRest);
    wheelRest -= whole;
    if (whole) setFirst(first + whole);
  }

  async function fetchPage(p: number) {
    if (pages.has(p) || loading.has(p)) return;
    loading.add(p);
    try {
      const reply = await pagedLines(id, p * PAGE, PAGE);
      pages.set(p, reply.page);
      if (reply.lines > available) available = reply.lines;
      complete = complete || reply.complete;
      evict();
      version++;
    } catch (e) {
      failure = String(e);
    } finally {
      loading.delete(p);
    }
  }

  function evict() {
    if (pages.size <= KEEP_PAGES) return;
    const here = Math.floor(first / PAGE);
    const far = [...pages.keys()].sort((a, b) => Math.abs(b - here) - Math.abs(a - here));
    for (const p of far.slice(0, pages.size - KEEP_PAGES)) pages.delete(p);
  }

  // Fetch what is visible plus half a page around it.
  $effect(() => {
    const lo = Math.max(0, first - PAGE / 2);
    const hi = Math.min(available, first + rows + PAGE / 2);
    for (let p = Math.floor(lo / PAGE); p * PAGE < hi; p++) fetchPage(p);
  });

  $effect(() => {
    void maxFirst;
    void spacer;
    tick().then(syncBar);
  });

  function reveal(line: number) {
    if (line < first || line >= first + rows) setFirst(line - Math.floor(rows / 3));
  }

  async function findNext() {
    if (!query || searching) return;
    searching = true;
    findStatus = "Searching…";
    const from = match ? { offset: match.byteOffset + 1 } : { line: current ?? first };
    try {
      const reply = await pagedFind(id, query, from, matchCase);
      if (reply.cancelled) return;
      if (reply.found) {
        match = reply.found;
        current = reply.found.line;
        findStatus = `Line ${(reply.found.line + 1).toLocaleString()}`;
        reveal(reply.found.line);
      } else {
        match = null;
        findStatus = complete ? "No match" : "No match in the lines read so far";
      }
    } catch (e) {
      findStatus = String(e);
    } finally {
      searching = false;
    }
  }

  function goTo() {
    const n = parseInt(gotoValue.replace(/[^\d]/g, ""), 10);
    if (!Number.isFinite(n) || available === 0) return;
    const line = Math.max(1, Math.min(available, n)) - 1;
    current = line;
    setFirst(line - Math.floor(rows / 3));
    gotoOpen = false;
    root.focus();
  }

  async function openFind() {
    findOpen = true;
    gotoOpen = false;
    await tick();
    findInput?.select();
  }

  async function openGoto() {
    gotoOpen = true;
    findOpen = false;
    await tick();
    gotoInput?.select();
  }

  function onKey(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && (e.key === "f" || e.key === "F")) {
      e.preventDefault();
      openFind();
      return;
    }
    if (mod && (e.key === "g" || e.key === "G")) {
      e.preventDefault();
      openGoto();
      return;
    }
    if (e.key === "F3") {
      e.preventDefault();
      findNext();
      return;
    }
    if (e.key === "Escape" && (findOpen || gotoOpen)) {
      findOpen = gotoOpen = false;
      root.focus();
      return;
    }
    if (e.target instanceof HTMLInputElement) return;
    const page = Math.max(1, rows - 1);
    const moves: Record<string, () => number> = {
      ArrowDown: () => first + 1,
      ArrowUp: () => first - 1,
      PageDown: () => first + page,
      PageUp: () => first - page,
      " ": () => first + (e.shiftKey ? -page : page),
      Home: () => 0,
      End: () => maxFirst,
    };
    const move = moves[e.key];
    if (move) {
      e.preventDefault();
      setFirst(move());
    }
  }

  // A new query starts from the view again.
  $effect(() => {
    void query;
    void matchCase;
    match = null;
    findStatus = "";
  });

  onMount(() => {
    const resize = new ResizeObserver(() => {
      rows = Math.max(1, Math.floor(content.clientHeight / ROW));
      syncBar();
    });
    resize.observe(content);
    content.addEventListener("wheel", onWheel, { passive: false });
    const unlisten = onIndexProgress((e) => {
      if (e.id !== id) return;
      if (e.lines > available) {
        // Pages cut short by the end of what was indexed are read again.
        for (const [p, lines] of pages) if (lines.length < PAGE) pages.delete(p);
        available = e.lines;
        version++;
      }
      complete = e.complete;
      if (e.error) failure = e.error;
      onprogress?.(e);
    });
    root.focus();
    return () => {
      resize.disconnect();
      content.removeEventListener("wheel", onWheel);
      unlisten.then((f) => f());
    };
  });

  const size = (n: number) =>
    n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(0)} KiB` : `${(n / 1048576).toFixed(1)} MiB`;
</script>

<div
  class="paged"
  bind:this={root}
  tabindex="0"
  onkeydown={onKey}
  role="textbox"
  aria-multiline="true"
  aria-readonly="true"
  aria-label="File content"
>
  {#if findOpen}
    <div class="bar-row">
      <input
        bind:this={findInput}
        bind:value={query}
        placeholder="Find"
        aria-label="Find"
        onkeydown={(e) => e.key === "Enter" && (e.preventDefault(), findNext())}
      />
      <label><input type="checkbox" bind:checked={matchCase} /> Match case</label>
      <button onclick={findNext} disabled={!query || searching}>Next</button>
      <span class="status" aria-live="polite">{findStatus}</span>
      <span class="grow"></span>
      <button onclick={() => ((findOpen = false), root.focus())} aria-label="Close find">Close</button>
    </div>
  {/if}
  {#if gotoOpen}
    <div class="bar-row">
      <label for="goto">Go to line</label>
      <input
        id="goto"
        bind:this={gotoInput}
        bind:value={gotoValue}
        inputmode="numeric"
        onkeydown={(e) => e.key === "Enter" && (e.preventDefault(), goTo())}
      />
      <span class="status">of {available.toLocaleString()}{complete ? "" : "+"}</span>
      <span class="grow"></span>
      <button onclick={() => ((gotoOpen = false), root.focus())}>Close</button>
    </div>
  {/if}
  {#if failure}
    <div class="bar-row error">{failure}</div>
  {/if}
  <div class="frame">
    <div class="content" bind:this={content}>
      {#each visible as r (r.n)}
        <div class="row" class:current={r.n === current}>
          <span class="num" style:min-width="{digits + 2}ch">{r.n + 1}</span>
          <span class="text"
            >{#if r.line}{#if match && match.line === r.n && match.column !== null}{r.line.text.slice(
                  0,
                  match.column,
                )}<mark>{r.line.text.slice(match.column, match.column + match.length)}</mark>{r.line.text.slice(
                  match.column + match.length,
                )}{:else}{r.line.text}{/if}{#if r.line.truncated}<span class="more"
                  >… {size(r.line.bytes)} line; the first {size(LINE_CAP)} are shown</span
                >{/if}{/if}</span
          >
        </div>
      {/each}
    </div>
    <div class="scroll" bind:this={bar} onscroll={onBarScroll} aria-hidden="true">
      <div style:height="{spacer}px"></div>
    </div>
  </div>
</div>

<style>
  .paged {
    display: flex;
    flex-direction: column;
    height: 100%;
    outline: none;
  }
  .frame {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .content {
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    overflow-y: hidden;
    font-family: var(--mono);
    font-size: 13px;
  }
  .row {
    display: flex;
    height: 20px;
    line-height: 20px;
    white-space: pre;
    tab-size: 4;
  }
  .row.current {
    background: var(--chip);
  }
  .num {
    position: sticky;
    left: 0;
    flex: none;
    padding: 0 8px 0 4px;
    text-align: right;
    color: var(--fg-muted);
    background: var(--bar);
    border-right: 1px solid var(--line);
    user-select: none;
  }
  .text {
    padding-left: 8px;
  }
  .more {
    color: var(--fg-muted);
    font-style: italic;
    font-family: var(--sans);
    font-size: 12px;
  }
  mark {
    background: var(--warn-bg);
    color: var(--warn);
    outline: 1px solid var(--warn);
  }
  .scroll {
    flex: none;
    width: 14px;
    overflow-y: scroll;
    overflow-x: hidden;
  }
  .bar-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
    font-size: 12.5px;
  }
  .bar-row.error {
    color: var(--danger);
  }
  .bar-row input:not([type="checkbox"]) {
    font: inherit;
    padding: 2px 6px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
    width: 240px;
  }
  .bar-row button {
    font: inherit;
    padding: 2px 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .status {
    color: var(--fg-muted);
  }
  .grow {
    flex: 1;
  }
</style>
