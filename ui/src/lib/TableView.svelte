<script lang="ts">
  // A table as a grid: delimited text or a bibliography. Rows come from Rust a page at a time,
  // in the order and with the filter chosen here, and only the visible ones are in the DOM.
  // The window of rows is drawn at the scroll position; for tables taller than a browser can
  // lay out, the scroll position is scaled to the row position.
  import { tick, untrack } from "svelte";
  import { copyText, tableArrange, tableRows, type Cell, type TableReady, type TableRow } from "./api";
  import { COPY_FORMATS, formatRows, type CopyFormat } from "./copyOut";

  let {
    table,
    stale = false,
    onopen,
    ondialect,
    onswitch,
  }: {
    table: TableReady;
    stale?: boolean;
    /// A cell was opened (double-click, Enter): show it in the text.
    onopen?: (cell: Cell) => void;
    ondialect?: (delimiter: string, header: boolean) => void;
    /// Another table of a multi-table file was chosen.
    onswitch?: (table: string) => void;
  } = $props();

  let schemaOpen = $state(false);
  const hasSchema = $derived(table.types !== null || table.details.length > 0 || table.schemaText !== null);

  const ROW = 26;
  const PAGE = 200;
  const KEEP_PAGES = 30;
  const MAX_HEIGHT = 8_000_000;

  let pages = new Map<number, TableRow[]>();
  const loading = new Set<number>();
  let version = $state(0);
  let scroller: HTMLDivElement;
  let scrollTop = $state(0);
  let height = $state(400);
  /// Selected cell: its position in the current order (null for the header) and column.
  let selected = $state<{ pos: number | null; column: number } | null>(null);
  let failure = $state<string | null>(null);

  // Order and filter, kept across re-indexing after edits.
  let sort = $state<{ column: number; descending: boolean } | null>(null);
  let filter = $state("");
  let shown = $state(0);
  /// Rows of a large binary file that the sort and filter covered (its first ones).
  let within = $state<number | null>(null);
  let arranging = $state(false);
  let filterTimer: ReturnType<typeof setTimeout> | undefined;

  const columns = $derived(Math.max(table.columns, table.header?.length ?? 0, 1));
  const total = $derived(shown * ROW);
  const scale = $derived(total > MAX_HEIGHT ? total / MAX_HEIGHT : 1);
  const exact = $derived(scrollTop * scale);
  const first = $derived(Math.min(Math.max(0, shown - 1), Math.floor(exact / ROW)));
  const visibleRows = $derived(Math.ceil(height / ROW) + 1);
  const numWidth = $derived(String(table.rows).length * 8 + 22);

  const rowAt = (pos: number): TableRow | undefined => pages.get(Math.floor(pos / PAGE))?.[pos % PAGE];

  // Column widths and numeric alignment from the header and the first page.
  const layout = $derived.by(() => {
    void version;
    const sample = pages.get(0) ?? [];
    const widths: number[] = [];
    const numeric: boolean[] = [];
    for (let c = 0; c < columns; c++) {
      let chars = Math.max((table.header?.[c]?.length ?? 0) + 2, 4);
      let numbers = 0;
      let filled = 0;
      for (const row of sample.slice(0, 100)) {
        const v = row.cells[c] ?? "null";
        chars = Math.max(chars, Math.min(v.length, 48));
        if (v.trim() !== "") {
          filled++;
          if (/^\s*[-+]?(\d[\d,_]*\.?\d*|\.\d+)([eE][-+]?\d+)?\s*$/.test(v)) numbers++;
        }
      }
      widths.push(Math.min(360, Math.max(60, chars * 8 + 24)));
      numeric.push(filled > 0 && numbers === filled);
    }
    return { widths, numeric, total: widths.reduce((a, b) => a + b, 0) };
  });

  const visible = $derived.by(() => {
    void version;
    const out: { pos: number; row: TableRow | null }[] = [];
    for (let pos = first; pos < Math.min(shown, first + visibleRows); pos++) {
      out.push({ pos, row: rowAt(pos) ?? null });
    }
    return out;
  });

  async function fetchPage(p: number) {
    if (pages.has(p) || loading.has(p) || arranging) return;
    const rev = table.rev;
    loading.add(p);
    try {
      const reply = await tableRows(rev, p * PAGE, PAGE);
      if (rev !== table.rev) return;
      pages.set(p, reply.rows);
      if (pages.size > KEEP_PAGES) {
        const here = Math.floor(first / PAGE);
        const far = [...pages.keys()].filter((k) => k !== 0).sort((a, b) => Math.abs(b - here) - Math.abs(a - here));
        for (const k of far.slice(0, pages.size - KEEP_PAGES)) pages.delete(k);
      }
      version++;
    } catch (e) {
      if (String(e) !== "stale") failure = String(e);
    } finally {
      loading.delete(p);
    }
  }

  /// Applies the order and filter in Rust, then reads rows again from the top.
  async function arrange() {
    const rev = table.rev;
    pages = new Map();
    loading.clear();
    within = null;
    arranging = true;
    try {
      const n = await tableArrange(rev, sort?.column ?? null, sort?.descending ?? false, filter);
      if (rev !== table.rev) return;
      shown = n.shown;
      within = n.within;
    } catch (e) {
      if (String(e) !== "stale") failure = String(e);
    } finally {
      arranging = false;
      version++;
    }
  }

  // A new table (opened, edited, another dialect): read it again with the same order and filter.
  $effect(() => {
    void table.rev;
    untrack(() => {
      failure = null;
      shown = table.rows;
      arrange();
    });
  });

  $effect(() => {
    void version;
    const lo = Math.floor(first / PAGE);
    const hi = Math.floor((first + visibleRows) / PAGE);
    for (let p = lo; p <= hi; p++) fetchPage(p);
  });

  function sortBy(column: number) {
    if (sort?.column !== column) sort = { column, descending: false };
    else if (!sort.descending) sort = { column, descending: true };
    else sort = null;
    selected = null;
    if (scroller) scroller.scrollTop = 0;
    arrange();
  }

  function onFilter(value: string) {
    filter = value;
    clearTimeout(filterTimer);
    filterTimer = setTimeout(() => {
      selected = null;
      if (scroller) scroller.scrollTop = 0;
      arrange();
    }, 250);
  }

  function scrollToPos(pos: number) {
    if (!scroller) return;
    const top = pos * ROW;
    if (top < exact || top + ROW > exact + height - ROW) {
      scroller.scrollTop = Math.max(0, (top - height / 3) / scale);
    }
  }

  function scrollToColumn(col: number) {
    if (!scroller) return;
    const left = layout.widths.slice(0, col).reduce((a, b) => a + b, 0);
    const right = left + layout.widths[col];
    const view = scroller.clientWidth - numWidth;
    if (left < scroller.scrollLeft) scroller.scrollLeft = left;
    else if (right > scroller.scrollLeft + view) scroller.scrollLeft = right - view;
  }

  /// Selects a cell and scrolls it into view. A row filtered out is not shown.
  export async function reveal(cell: Cell) {
    const pos = cell.row === null ? null : (cell.position ?? (sort || filter ? null : cell.row));
    if (cell.row !== null && pos === null) return;
    selected = { pos, column: cell.column };
    await tick();
    if (pos !== null) scrollToPos(pos);
    scrollToColumn(cell.column);
  }

  function open() {
    if (!selected) return;
    if (selected.pos === null) {
      if (table.header && table.dialect) onopen?.({ row: null, column: selected.column });
      return;
    }
    const row = rowAt(selected.pos);
    if (row) onopen?.({ row: row.n, column: selected.column });
  }

  /// Rows copied at most, from the top of the current order and filter.
  const COPY_MAX = 10_000;
  let copyNote = $state<string | null>(null);
  let noteTimer: ReturnType<typeof setTimeout> | undefined;

  function note(text: string) {
    copyNote = text;
    clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (copyNote = null), 4000);
  }

  async function copyRows(format: CopyFormat) {
    const rev = table.rev;
    const want = Math.min(shown, COPY_MAX);
    const rows: (string | null)[][] = [];
    try {
      while (rows.length < want) {
        const reply = await tableRows(rev, rows.length, Math.min(2000, want - rows.length));
        if (rev !== table.rev) return;
        if (reply.rows.length === 0) break;
        rows.push(...reply.rows.map((r) => r.cells));
      }
      await copyText(formatRows(table.header, rows, format));
      const n = rows.length;
      note(`Copied ${n.toLocaleString()} ${n === 1 ? "row" : "rows"}${shown > COPY_MAX ? `, the first ${COPY_MAX.toLocaleString()}` : ""}`);
    } catch (e) {
      note(`Not copied: ${e}`);
    }
  }

  function copyCell() {
    if (!selected) return;
    const value = selected.pos === null ? table.header?.[selected.column] : rowAt(selected.pos)?.cells[selected.column];
    if (value === undefined) return;
    copyText(value ?? "").then(
      () => note("Copied the cell"),
      (e) => note(`Not copied: ${e}`),
    );
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement) return;
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && (e.key === "c" || e.key === "C") && selected) {
      e.preventDefault();
      copyCell();
      return;
    }
    const s = selected ?? { pos: 0, column: 0 };
    const pos = s.pos ?? -1;
    const page = Math.max(1, visibleRows - 2);
    const last = shown - 1;
    const move: Record<string, () => { pos: number | null; column: number }> = {
      ArrowDown: () => ({ pos: Math.min(last, pos + 1), column: s.column }),
      ArrowUp: () => ({ pos: pos <= 0 ? (table.header ? null : 0) : pos - 1, column: s.column }),
      ArrowRight: () => ({ pos: s.pos, column: Math.min(columns - 1, s.column + 1) }),
      ArrowLeft: () => ({ pos: s.pos, column: Math.max(0, s.column - 1) }),
      PageDown: () => ({ pos: Math.min(last, pos + page), column: s.column }),
      PageUp: () => ({ pos: Math.max(0, pos - page), column: s.column }),
      Home: () => (e.ctrlKey || e.metaKey ? { pos: 0, column: 0 } : { pos: s.pos, column: 0 }),
      End: () => (e.ctrlKey || e.metaKey ? { pos: last, column: columns - 1 } : { pos: s.pos, column: columns - 1 }),
    };
    if (move[e.key]) {
      e.preventDefault();
      selected = move[e.key]();
      if (selected.pos !== null) scrollToPos(selected.pos);
      scrollToColumn(selected.column);
    } else if (e.key === "Enter" && selected) {
      e.preventDefault();
      open();
    }
  }

  const DELIMITERS: [string, string][] = [
    [",", "Comma"],
    [";", "Semicolon"],
    ["\t", "Tab"],
    ["|", "Pipe"],
  ];
  const columnName = (c: number) => table.header?.[c] ?? `${c + 1}`;
  /// Line breaks inside a cell shown as a symbol, so every row keeps one line.
  const oneLine = (v: string) => (v.includes("\n") ? v.replace(/\r?\n/g, " ↵ ") : v);
</script>

<div class="table" class:stale>
  <div class="tools">
    {#if table.dialect}
      {@const dialect = table.dialect}
      <label>
        Delimiter
        <select
          value={dialect.delimiter}
          onchange={(e) => ondialect?.((e.currentTarget as HTMLSelectElement).value, dialect.hasHeader)}
        >
          {#each DELIMITERS as [d, name] (d)}<option value={d}>{name}</option>{/each}
          {#if !DELIMITERS.some(([d]) => d === dialect.delimiter)}
            <option value={dialect.delimiter}>{JSON.stringify(dialect.delimiter)}</option>
          {/if}
        </select>
      </label>
      <label>
        <input
          type="checkbox"
          checked={dialect.hasHeader}
          onchange={(e) => ondialect?.(dialect.delimiter, (e.currentTarget as HTMLInputElement).checked)}
        />
        First row is a header
      </label>
    {/if}
    {#if table.tables.length > 1}
      <label>
        Table
        <select
          value={table.tables[table.table]?.name}
          onchange={(e) => onswitch?.((e.currentTarget as HTMLSelectElement).value)}
        >
          {#each table.tables as t (t.name)}
            <option value={t.name}>{t.name}{t.rows !== null ? ` (${t.rows.toLocaleString()} rows)` : ""}</option>
          {/each}
        </select>
      </label>
    {/if}
    {#if table.arrangeable}
      <input
        class="filter"
        type="search"
        placeholder="Filter rows"
        aria-label="Filter rows"
        value={filter}
        oninput={(e) => onFilter((e.currentTarget as HTMLInputElement).value)}
      />
    {/if}
    {#if filter.trim() !== "" && !arranging}
      <span class="muted"
        >{shown.toLocaleString()} of {(within ?? table.rows).toLocaleString()}{within !== null ? " first" : ""} rows</span
      >
    {/if}
    {#if within !== null && !arranging}
      <span class="muted" title="Large files are sorted and filtered over their first rows, read into memory"
        >Sorted and filtered within the first {within.toLocaleString()} of {table.rows.toLocaleString()} rows</span
      >
    {/if}
    <span class="grow"></span>
    {#if stale || arranging}<span class="muted" aria-live="polite">Updating…</span>{/if}
    {#if copyNote}<span class="muted" aria-live="polite">{copyNote}</span>{/if}
    <select
      class="copy"
      aria-label="Copy rows as"
      title="Copy the rows shown (up to {COPY_MAX.toLocaleString()}) in this order. Ctrl+C copies the selected cell."
      value=""
      disabled={stale || arranging || shown === 0}
      onchange={(e) => {
        const el = e.currentTarget as HTMLSelectElement;
        const format = el.value as CopyFormat;
        el.value = "";
        if (format) copyRows(format);
      }}
    >
      <option value="" disabled>Copy as…</option>
      {#each COPY_FORMATS as f (f.id)}<option value={f.id}>{f.name}</option>{/each}
    </select>
    {#if failure}<span class="error">{failure}</span>{/if}
    {#if table.error}<span class="error">{table.error}</span>{/if}
    {#if hasSchema}
      <button class="schema-toggle" class:on={schemaOpen} onclick={() => (schemaOpen = !schemaOpen)} aria-pressed={schemaOpen}
        >Schema</button
      >
    {/if}
  </div>
  <div class="body">
  <div
    class="scroller"
    bind:this={scroller}
    bind:clientHeight={height}
    onscroll={() => (scrollTop = scroller.scrollTop)}
    tabindex="0"
    role="grid"
    aria-label="Table"
    aria-rowcount={shown + 1}
    aria-colcount={columns}
    onkeydown={onKey}
  >
    <div class="spacer" style:height="{Math.min(total, MAX_HEIGHT) + ROW}px" style:width="{layout.total + numWidth}px">
      <div class="window" style:top="{scrollTop}px">
        <div class="row head" role="row">
          <span class="num corner" style:width="{numWidth}px"></span>
          {#each layout.widths as w, c (c)}
            <button
              class="cell"
              class:numeric={layout.numeric[c]}
              class:selected={selected?.pos === null && selected?.column === c}
              style:width="{w}px"
              role="columnheader"
              aria-sort={sort?.column === c ? (sort.descending ? "descending" : "ascending") : "none"}
              title={table.arrangeable
                ? `Sort by ${columnName(c)}${table.types?.[c] ? ` (${table.types[c]})` : ""}`
                : `${columnName(c)}${table.types?.[c] ? `: ${table.types[c]}` : ""}`}
              tabindex="-1"
              onclick={() => table.arrangeable && sortBy(c)}
              >{columnName(c)}{#if sort?.column === c}<span class="arrow">{sort.descending ? " ▼" : " ▲"}</span>{/if}</button
            >
          {/each}
        </div>
        <div class="rows" style:transform="translateY({-(exact % ROW)}px)">
          {#each visible as r (r.pos)}
            <div class="row" role="row" aria-rowindex={r.pos + 2}>
              <span class="num" style:width="{numWidth}px">{r.row ? r.row.n + 1 : ""}</span>
              {#each layout.widths as w, c (c)}
                {@const v = r.row?.cells[c]}
                <span
                  class="cell"
                  class:numeric={layout.numeric[c]}
                  class:missing={r.row !== null && v === undefined}
                  class:selected={selected?.pos === r.pos && selected?.column === c}
                  style:width="{w}px"
                  role="gridcell"
                  title={v && v.length > 30 ? v : undefined}
                  onclick={() => (selected = { pos: r.pos, column: c })}
                  ondblclick={() => ((selected = { pos: r.pos, column: c }), open())}
                  onkeydown={() => {}}
                  tabindex="-1"
                  >{#if v === null}<span class="null">null</span>{:else if v !== undefined}{oneLine(v)}{/if}</span
                >
              {/each}
            </div>
          {/each}
          {#if shown === 0 && !arranging}
            <p class="none">{filter.trim() ? "No row contains this text." : "No rows."}</p>
          {/if}
        </div>
      </div>
    </div>
  </div>
  {#if schemaOpen}
    <aside class="schema" aria-label="Schema">
      {#if table.types}
        <h3>Columns</h3>
        <table>
          <tbody>
            {#each table.header ?? [] as name, i (i)}
              <tr><td>{name}</td><td class="type">{table.types[i] ?? ""}</td></tr>
            {/each}
          </tbody>
        </table>
      {/if}
      {#if table.details.length > 0}
        <h3>File</h3>
        <dl>
          {#each table.details as [k, v] (k)}<dt>{k}</dt><dd>{v}</dd>{/each}
        </dl>
      {/if}
      {#if table.schemaText}
        <h3>As written</h3>
        <pre>{table.schemaText}</pre>
      {/if}
    </aside>
  {/if}
  </div>
</div>

<style>
  .table {
    display: flex;
    flex-direction: column;
    height: 100%;
    font-size: 12.5px;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
  }
  .tools select {
    margin-left: 6px;
  }
  .filter {
    width: 220px;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .scroller {
    flex: 1;
    min-width: 0;
    overflow: auto;
    outline: none;
    position: relative;
  }
  .schema {
    width: 320px;
    flex: none;
    overflow: auto;
    border-left: 1px solid var(--line);
    padding: 10px 14px;
    background: var(--bg);
  }
  .schema h3 {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-muted);
    margin: 12px 0 6px;
  }
  .schema table {
    border-collapse: collapse;
    width: 100%;
  }
  .schema td {
    padding: 2px 6px 2px 0;
    font-family: var(--mono);
    vertical-align: top;
    overflow-wrap: anywhere;
  }
  .schema .type {
    color: var(--syn-type);
  }
  .schema dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 12px;
    margin: 0;
  }
  .schema dt {
    color: var(--fg-muted);
  }
  .schema dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .schema pre {
    font-size: 11.5px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--bar);
    padding: 8px;
    border-radius: 6px;
  }
  .schema-toggle {
    font: inherit;
    font-size: 12.5px;
    height: 28px;
    padding: 0 11px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
    cursor: pointer;
  }
  .schema-toggle.on {
    background: var(--chip);
    font-weight: 600;
  }
  .null {
    color: var(--fg-muted);
    font-style: italic;
  }
  .spacer {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
  }
  .stale .rows {
    opacity: 0.6;
  }
  .row {
    display: flex;
    height: 26px;
    line-height: 25px;
    border-bottom: 1px solid var(--line);
  }
  .head {
    position: relative;
    z-index: 2;
    background: var(--bar);
    font-weight: 600;
  }
  .num {
    position: sticky;
    left: 0;
    z-index: 1;
    flex: none;
    padding: 0 8px;
    text-align: right;
    color: var(--fg-muted);
    background: var(--bar);
    border-right: 1px solid var(--line);
    font-family: var(--mono);
    font-size: 11.5px;
    user-select: none;
  }
  .cell {
    flex: none;
    padding: 0 8px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    border-right: 1px solid var(--line);
    font-family: var(--mono);
    cursor: default;
  }
  .head .cell {
    font: inherit;
    font-family: var(--sans);
    font-weight: 600;
    text-align: left;
    color: var(--fg);
    background: none;
    border-top: none;
    border-bottom: none;
    border-left: none;
    border-radius: 0;
    cursor: pointer;
  }
  .head .cell:hover {
    background: var(--chip);
  }
  .arrow {
    color: var(--accent);
  }
  .cell.numeric {
    text-align: right;
  }
  .cell.missing {
    background: repeating-linear-gradient(45deg, transparent 0 6px, var(--chip) 6px 7px);
  }
  .cell.selected {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
    background: var(--node);
  }
  .none {
    padding: 16px;
    color: var(--fg-muted);
    font-family: var(--sans);
  }
</style>
