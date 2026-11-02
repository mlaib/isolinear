<script lang="ts">
  // Any file as hex rows: offset, sixteen bytes, and their printable characters. Rows are read
  // from the file a page at a time; the scroll position is scaled for files of any size.
  import { untrack } from "svelte";
  import { hexRows } from "./api";

  // `at`: an offset to show and mark when the view opens.
  let { path, size, at = null }: { path: string; size: number; at?: number | null } = $props();

  const ROW = 20;
  const PAGE = 512;
  const KEEP_PAGES = 16;
  const MAX_HEIGHT = 8_000_000;

  const rows = $derived(Math.ceil(size / 16));
  const total = $derived(rows * ROW);
  const scale = $derived(total > MAX_HEIGHT ? total / MAX_HEIGHT : 1);
  const digits = $derived(Math.max(8, size.toString(16).length));

  let pages = new Map<number, number[]>();
  const loading = new Set<number>();
  let version = $state(0);
  let scroller: HTMLDivElement;
  let scrollTop = $state(0);
  let height = $state(400);
  let failure = $state<string | null>(null);
  let gotoOpen = $state(false);
  let gotoValue = $state("");
  let marked = $state<number | null>(null);

  const exact = $derived(scrollTop * scale);
  const first = $derived(Math.min(Math.max(0, rows - 1), Math.floor(exact / ROW)));
  const visible = $derived(Math.ceil(height / ROW) + 1);

  const shown = $derived.by(() => {
    void version;
    const out: { n: number; bytes: number[] | null }[] = [];
    for (let n = first; n < Math.min(rows, first + visible); n++) {
      const page = pages.get(Math.floor(n / PAGE));
      const at = (n % PAGE) * 16;
      out.push({ n, bytes: page ? page.slice(at, at + 16) : null });
    }
    return out;
  });

  async function fetchPage(p: number) {
    if (pages.has(p) || loading.has(p)) return;
    loading.add(p);
    try {
      const reply = await hexRows(path, p * PAGE, PAGE);
      pages.set(p, reply.bytes);
      if (pages.size > KEEP_PAGES) {
        const here = Math.floor(first / PAGE);
        const far = [...pages.keys()].sort((a, b) => Math.abs(b - here) - Math.abs(a - here));
        for (const k of far.slice(0, pages.size - KEEP_PAGES)) pages.delete(k);
      }
      version++;
    } catch (e) {
      failure = String(e);
    } finally {
      loading.delete(p);
    }
  }

  $effect(() => {
    void path;
    untrack(() => {
      pages = new Map();
      version++;
    });
  });

  $effect(() => {
    void version;
    for (let p = Math.floor(first / PAGE); p <= Math.floor((first + visible) / PAGE); p++) fetchPage(p);
  });

  const hex = (b: number) => b.toString(16).toUpperCase().padStart(2, "0");
  const printable = (b: number) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "·");

  function jump(n: number) {
    if (!Number.isFinite(n) || n < 0 || n >= size) return;
    marked = n;
    scroller.scrollTop = Math.max(0, (Math.floor(n / 16) * ROW - height / 3) / scale);
  }

  function goTo() {
    const v = gotoValue.trim().toLowerCase();
    const n = v.startsWith("0x") ? parseInt(v.slice(2), 16) : /^[0-9a-f]+h$/.test(v) ? parseInt(v, 16) : parseInt(v, 10);
    if (!Number.isFinite(n) || n < 0 || n >= size) return;
    jump(n);
    gotoOpen = false;
  }

  $effect(() => {
    const n = at;
    if (n !== null) untrack(() => jump(n));
  });

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && (e.key === "g" || e.key === "G")) {
      e.preventDefault();
      gotoOpen = true;
    } else if (e.key === "Escape") {
      gotoOpen = false;
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="hex" onkeydown={onKey}>
  {#if gotoOpen}
    <div class="bar">
      <label for="goto-offset">Go to offset</label>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        id="goto-offset"
        bind:value={gotoValue}
        placeholder="1024 or 0x400"
        autofocus
        onkeydown={(e) => e.key === "Enter" && (e.preventDefault(), goTo())}
      />
      <span class="muted">of {size.toLocaleString()} bytes</span>
    </div>
  {/if}
  {#if failure}<div class="bar error">{failure}</div>{/if}
  <div
    class="scroller"
    bind:this={scroller}
    bind:clientHeight={height}
    onscroll={() => (scrollTop = scroller.scrollTop)}
    tabindex="0"
    role="textbox"
    aria-readonly="true"
    aria-multiline="true"
    aria-label="Bytes, in hexadecimal. Ctrl+G goes to an offset."
  >
    <div class="spacer" style:height="{Math.min(total, MAX_HEIGHT)}px">
      <div class="window" style:top="{scrollTop}px" style:transform="translateY({-(exact % ROW)}px)">
        {#each shown as r (r.n)}
          <div class="row">
            <span class="offset">{(r.n * 16).toString(16).toUpperCase().padStart(digits, "0")}</span>
            <span class="bytes"
              >{#if r.bytes}{#each r.bytes as b, i (i)}<span
                    class:gap={i === 8}
                    class:zero={b === 0}
                    class:mark={marked !== null && r.n * 16 + i === marked}>{hex(b)}</span
                  >{/each}{/if}</span
            >
            <span class="ascii">{r.bytes ? r.bytes.map(printable).join("") : ""}</span>
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .hex {
    display: flex;
    flex-direction: column;
    height: 100%;
    font-family: var(--mono);
    font-size: 13px;
  }
  .bar {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
    font-family: var(--sans);
    font-size: 12.5px;
  }
  .bar input {
    font: inherit;
    padding: 2px 6px;
    border: 1px solid var(--line);
    border-radius: 5px;
    background: var(--bg);
    color: var(--fg);
  }
  .error {
    color: var(--danger);
  }
  .muted {
    color: var(--fg-muted);
  }
  .scroller {
    flex: 1;
    min-height: 0;
    overflow: auto;
    outline: none;
  }
  .spacer {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
  }
  .row {
    height: 20px;
    line-height: 20px;
    display: flex;
    gap: 24px;
    padding-left: 12px;
    white-space: pre;
  }
  .offset {
    color: var(--fg-muted);
    user-select: none;
  }
  .bytes span {
    margin-right: 0.6ch;
  }
  .bytes .gap {
    margin-left: 0.8ch;
  }
  .bytes .zero {
    color: var(--fg-muted);
    opacity: 0.6;
  }
  .bytes .mark {
    background: var(--warn-bg);
    color: var(--warn);
    outline: 1px solid var(--warn);
  }
  .ascii {
    color: var(--syn-string);
  }
</style>
