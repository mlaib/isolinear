<script lang="ts">
  // The structure tree, loaded lazily from Rust: children a page at a time, only visible rows in
  // the DOM. Expansion is remembered by position ("0/3/1"), so it survives re-parsing after an
  // edit. Picking a row reports its source range; `reveal` follows the editor's cursor.
  import { tick, untrack } from "svelte";
  import { treeChildren, type NodeKind, type NodeRow, type PathStep } from "./api";

  let {
    rev,
    stale = false,
    onpick,
    oncopy,
  }: {
    rev: number | null;
    stale?: boolean;
    onpick?: (row: NodeRow) => void;
    /// Ctrl/Cmd+C on a node: copy its text as written.
    oncopy?: (row: NodeRow) => void;
  } = $props();

  const ROW = 22;
  const PAGE = 500;
  /// Furthest child position `reveal` loads pages up to.
  const REVEAL_MAX = 50_000;

  type Kids = { rows: NodeRow[]; total: number };
  type Row =
    | { type: "node"; node: NodeRow; key: string; depth: number; index: number; parent: NodeKind }
    | { type: "more"; parent: number; key: string; depth: number; shown: number; total: number };

  let kids = new Map<number, Kids>();
  const expanded = new Set<string>();
  const inflight = new Map<string, Promise<void>>();
  let version = $state(0);
  let selected: string | null = $state(null);
  let scroller: HTMLDivElement;
  let height = $state(400);
  let scrollTop = $state(0);
  let failure: string | null = $state(null);

  const rows = $derived.by(() => {
    void version;
    const out: Row[] = [];
    const root = kids.get(0);
    if (!root) return out;
    const walk = (parentId: number, parent: NodeKind, prefix: string, depth: number, list: Kids) => {
      list.rows.forEach((node, index) => {
        const key = prefix ? `${prefix}/${index}` : `${index}`;
        out.push({ type: "node", node, key, depth, index, parent });
        const sub = expanded.has(key) ? kids.get(node.id) : undefined;
        if (sub) walk(node.id, node.kind, key, depth + 1, sub);
      });
      if (list.rows.length < list.total) {
        out.push({ type: "more", parent: parentId, key: `${prefix}/+`, depth, shown: list.rows.length, total: list.total });
      }
    };
    walk(0, "document", "", 0, root);
    return out;
  });

  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 5));
  const shown = $derived(rows.slice(first, first + Math.ceil(height / ROW) + 10));

  function load(id: number, more = false): Promise<void> {
    const r = rev;
    if (r === null) return Promise.resolve();
    const have = more ? (kids.get(id)?.rows.length ?? 0) : 0;
    const key = `${id}:${have}`;
    const running = inflight.get(key);
    if (running) return running;
    const job = treeChildren(r, id, have, PAGE)
      .then((reply) => {
        if (r !== rev) return;
        const previous = more ? (kids.get(id)?.rows ?? []) : [];
        kids.set(id, { rows: [...previous, ...reply.rows], total: reply.total });
        // A document with a single value opens with that value expanded.
        if (id === 0 && reply.rows.length === 1 && reply.rows[0].children > 0) expanded.add("0");
        version++;
      })
      .catch((e) => {
        if (String(e) !== "stale") failure = String(e);
      })
      .finally(() => inflight.delete(key));
    inflight.set(key, job);
    return job;
  }

  // A new revision: forget loaded children, keep what is expanded.
  $effect(() => {
    const r = rev;
    untrack(() => {
      kids = new Map();
      inflight.clear();
      failure = null;
      version++;
      if (r !== null) load(0);
    });
  });

  // Expanded rows whose children are not loaded yet (after a re-parse) load them.
  $effect(() => {
    for (const row of rows) {
      if (row.type === "node" && row.node.children > 0 && expanded.has(row.key) && !kids.has(row.node.id)) {
        load(row.node.id);
      }
    }
  });

  function toggle(row: Row & { type: "node" }) {
    if (row.node.children === 0) return;
    if (expanded.has(row.key)) expanded.delete(row.key);
    else {
      expanded.add(row.key);
      if (!kids.has(row.node.id)) load(row.node.id);
    }
    version++;
  }

  function pick(row: Row & { type: "node" }) {
    selected = row.key;
    if (!stale) onpick?.(row.node);
  }

  /// Expands the path to a node, selects it and scrolls it into view.
  export async function reveal(path: PathStep[]) {
    const r = rev;
    let key = "";
    for (let i = 1; i < path.length; i++) {
      const parent = path[i - 1].id;
      const step = path[i];
      if (!kids.has(parent)) await load(parent);
      let list = kids.get(parent);
      while (list && list.rows.length <= step.index && list.rows.length < list.total && step.index < REVEAL_MAX) {
        await load(parent, true);
        list = kids.get(parent);
      }
      if (r !== rev || !list || list.rows.length <= step.index) break;
      if (key) expanded.add(key);
      key = key ? `${key}/${step.index}` : `${step.index}`;
    }
    if (!key) return;
    version++;
    selected = key;
    await tick();
    scrollToKey(key);
  }

  function scrollToKey(key: string) {
    const i = rows.findIndex((r) => r.key === key);
    if (i < 0 || !scroller) return;
    const top = i * ROW;
    if (top < scroller.scrollTop || top + ROW > scroller.scrollTop + height) {
      scroller.scrollTop = Math.max(0, top - height / 3);
    }
  }

  function onKey(e: KeyboardEvent) {
    const nodes = rows.filter((r): r is Row & { type: "node" } => r.type === "node");
    const i = nodes.findIndex((r) => r.key === selected);
    const current = nodes[i];
    const go = (r: (Row & { type: "node" }) | undefined) => {
      if (!r) return;
      e.preventDefault();
      pick(r);
      scrollToKey(r.key);
    };
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && (e.key === "c" || e.key === "C")) {
      if (current && !stale) {
        e.preventDefault();
        oncopy?.(current.node);
      }
      return;
    }
    switch (e.key) {
      case "ArrowDown":
        go(nodes[Math.min(nodes.length - 1, i + 1)]);
        break;
      case "ArrowUp":
        go(nodes[Math.max(0, i - 1)]);
        break;
      case "Home":
        go(nodes[0]);
        break;
      case "End":
        go(nodes[nodes.length - 1]);
        break;
      case "ArrowRight":
        if (!current) return go(nodes[0]);
        e.preventDefault();
        if (current.node.children > 0 && !expanded.has(current.key)) toggle(current);
        else if (expanded.has(current.key)) go(nodes[i + 1]);
        break;
      case "ArrowLeft":
        if (!current) return;
        e.preventDefault();
        if (expanded.has(current.key)) toggle(current);
        else go(nodes.find((r) => r.key === current.key.split("/").slice(0, -1).join("/")));
        break;
      case "Enter":
      case " ":
        if (current) {
          e.preventDefault();
          toggle(current);
        }
        break;
    }
  }

  function label(r: Row & { type: "node" }): { name: string | null; cls: string } {
    const n = r.node;
    switch (n.kind) {
      case "record":
        return { name: `Line ${n.line}`, cls: "key" };
      case "element":
        return { name: `<${n.name ?? ""}>`, cls: "tag" };
      case "attribute":
        return { name: n.name, cls: "attr" };
      case "text":
        return { name: "text", cls: "muted" };
      case "cData":
        return { name: "CDATA", cls: "muted" };
      case "comment":
        return { name: "comment", cls: "muted" };
      case "processingInstruction":
        return { name: `<?${n.name ?? ""}?>`, cls: "muted" };
      case "doctype":
        return { name: "DOCTYPE", cls: "muted" };
      case "part":
        return { name: `Document ${r.index + 1}`, cls: "key" };
      case "table":
        return { name: n.name ?? "(top)", cls: "key" };
      case "section":
        return { name: n.name ?? "(untitled)", cls: "heading" };
      case "symbol":
        return { name: n.name ?? "(anonymous)", cls: "symbol" };
    }
    if (n.name !== null) return { name: n.name, cls: "key" };
    if (r.parent === "array") return { name: `[${r.index}]`, cls: "index" };
    return { name: null, cls: "key" };
  }

  function summary(n: NodeRow): string | null {
    switch (n.kind) {
      case "object":
        return `{${n.children}}`;
      case "array":
        return `[${n.children}]`;
      case "table":
        return `${n.children} ${n.children === 1 ? "key" : "keys"}`;
      case "element":
        return n.children > 0 ? `${n.children}` : null;
      default:
        return null;
    }
  }
</script>

<div class="tree" class:stale>
  {#if stale}
    <div class="note" aria-live="polite">Updating structure…</div>
  {/if}
  {#if failure}
    <div class="note error">{failure}</div>
  {/if}
  <div
    class="scroller"
    bind:this={scroller}
    bind:clientHeight={height}
    onscroll={() => (scrollTop = scroller.scrollTop)}
    tabindex="0"
    role="tree"
    aria-label="Structure"
    onkeydown={onKey}
  >
    <div class="spacer" style:height="{rows.length * ROW}px">
      {#each shown as r, i (r.key)}
        {@const top = (first + i) * ROW}
        {#if r.type === "node"}
          {@const l = label(r)}
          {@const sum = summary(r.node)}
          <div
            class="row"
            class:selected={r.key === selected}
            style:top="{top}px"
            style:padding-left="{6 + r.depth * 14}px"
            role="treeitem"
            aria-selected={r.key === selected}
            aria-expanded={r.node.children > 0 ? expanded.has(r.key) : undefined}
            aria-level={r.depth + 1}
            tabindex="-1"
            onclick={() => pick(r)}
            ondblclick={() => toggle(r)}
            onkeydown={() => {}}
          >
            <button
              class="twisty"
              class:open={expanded.has(r.key)}
              class:leaf={r.node.children === 0}
              tabindex="-1"
              aria-hidden="true"
              onclick={(e) => (e.stopPropagation(), toggle(r))}
            ></button>
            {#if l.name !== null}<span class={l.cls}
                >{l.name}{#if r.node.value !== null && l.cls !== "muted" && r.node.kind !== "symbol"}<span class="muted">:</span>{/if}</span
              >{/if}
            {#if r.node.value !== null && r.node.kind === "symbol"}
              <span class="muted">{r.node.value}</span>
            {:else if r.node.value !== null}
              <span class="value {r.node.scalar ?? r.node.kind}">{r.node.value}</span>
            {/if}
            {#if sum}<span class="summary">{sum}</span>{/if}
          </div>
        {:else}
          <div class="row" style:top="{top}px" style:padding-left="{26 + r.depth * 14}px">
            <button class="more" onclick={() => load(r.parent, true)}>
              Show more ({r.shown.toLocaleString()} of {r.total.toLocaleString()})
            </button>
          </div>
        {/if}
      {/each}
    </div>
  </div>
</div>

<style>
  .tree {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
    font-size: 12.5px;
  }
  .note {
    padding: 4px 10px;
    color: var(--fg-muted);
    border-bottom: 1px solid var(--line);
    background: var(--bar);
  }
  .note.error {
    color: var(--danger);
  }
  .scroller {
    flex: 1;
    min-height: 0;
    overflow: auto;
    outline: none;
  }
  .spacer {
    position: relative;
    min-width: 100%;
  }
  .stale .spacer {
    opacity: 0.6;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    height: 22px;
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: pre;
    cursor: default;
    font-family: var(--mono);
    font-size: 12.5px;
  }
  .row:hover {
    background: var(--active-line);
  }
  .row.selected {
    background: var(--selection);
  }
  .scroller:focus-visible .row.selected {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .twisty {
    width: 16px;
    height: 16px;
    flex: none;
    padding: 0;
    border: none;
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .twisty::before {
    content: "";
    display: inline-block;
    border: 4px solid transparent;
    border-left: 5px solid currentColor;
    margin-left: 4px;
    transition: transform 0.1s;
    transform-origin: 2px 4px;
  }
  .twisty.open::before {
    transform: rotate(90deg);
  }
  .twisty.leaf {
    visibility: hidden;
  }
  .key {
    color: var(--syn-property);
  }
  .index {
    color: var(--fg-muted);
  }
  .symbol {
    color: var(--syn-function);
  }
  .heading {
    color: var(--fg);
    font-family: var(--sans);
  }
  .tag {
    color: var(--syn-tag);
  }
  .attr {
    color: var(--syn-attribute);
  }
  .muted {
    color: var(--fg-muted);
  }
  .value {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .value.string,
  .value.text,
  .value.cData {
    color: var(--syn-string);
  }
  .value.number,
  .value.boolean,
  .value.null,
  .value.date {
    color: var(--syn-number);
  }
  .value.comment {
    color: var(--syn-comment);
    font-style: italic;
  }
  .summary {
    color: var(--fg-muted);
    font-size: 11.5px;
    margin-left: 4px;
  }
  .more {
    font: inherit;
    font-family: var(--sans);
    font-size: 12px;
    border: none;
    background: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
  }
</style>
