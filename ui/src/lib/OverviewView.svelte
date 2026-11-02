<script lang="ts">
  // What an industry-standard reader found: the standard, what was and was not checked, facts,
  // tables, an outline and pictures of the content. Anything with a source shows its text when
  // clicked (text files). Nothing is fetched: maps have no background, pictures are the file's.
  import { inspectDocument, type Overview, type OverviewSource, type OverviewVisual } from "./api";

  let {
    path,
    format,
    onsource,
  }: { path: string; format: string; onsource?: (s: OverviewSource) => void } = $props();

  let data = $state<Overview | null>(null);
  let failure = $state<string | null>(null);
  let shownRows = $state<Record<number, number>>({});

  $effect(() => {
    const p = path;
    data = null;
    failure = null;
    shownRows = {};
    inspectDocument(p, format)
      .then((o) => {
        if (p !== path) return;
        if (o === null) failure = "No standard was recognized in this file.";
        else data = o;
      })
      .catch((e) => (failure = String(e)));
  });

  const groups = $derived.by(() => {
    const out = new Map<string, Overview["facts"]>();
    for (const f of data?.facts ?? []) {
      const g = out.get(f.group) ?? [];
      g.push(f);
      out.set(f.group, g);
    }
    return [...out];
  });

  const STATE: Record<string, [string, string]> = {
    passed: ["✓", "Passed"],
    partial: ["!", "In part"],
    failed: ["✗", "Failed"],
    notDone: ["–", "Not done"],
  };

  const linked = (s: OverviewSource | null) => s !== null && s.text !== null && !!onsource;

  // Fits coordinates into a box; geographic maps use longitude scaled by the latitude.
  function fit(points: [number, number][], w: number, h: number, geographic: boolean) {
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const [x, y] of points) {
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue;
      x0 = Math.min(x0, x);
      x1 = Math.max(x1, x);
      y0 = Math.min(y0, y);
      y1 = Math.max(y1, y);
    }
    if (!Number.isFinite(x0)) return (p: [number, number]) => [0, 0] as [number, number];
    const k = geographic ? Math.cos((((y0 + y1) / 2) * Math.PI) / 180) || 1 : 1;
    const sx = (x1 - x0) * k || 1;
    const sy = y1 - y0 || 1;
    const s = Math.min((w - 20) / sx, (h - 20) / sy);
    const ox = (w - sx * s) / 2;
    const oy = (h - sy * s) / 2;
    return ([x, y]: [number, number]) => [ox + (x - x0) * k * s, h - (oy + (y - y0) * s)] as [number, number];
  }

  const poly = (pts: [number, number][], f: (p: [number, number]) => [number, number]) =>
    pts.map((p) => f(p).map((v) => v.toFixed(1)).join(",")).join(" ");

  function drawImage(canvas: HTMLCanvasElement, v: OverviewVisual & { kind: "image" }) {
    const bytes = Uint8ClampedArray.from(atob(v.rgba), (c) => c.charCodeAt(0));
    canvas.width = v.width;
    canvas.height = v.height;
    canvas.getContext("2d")?.putImageData(new ImageData(bytes, v.width, v.height), 0, 0);
  }

  // A wireframe seen from an angle the user turns by dragging.
  function drawMesh(canvas: HTMLCanvasElement, v: OverviewVisual & { kind: "mesh" }) {
    let yaw = 0.6;
    let pitch = 0.5;
    const n = v.vertices.length / 3;
    let [cx, cy, cz, r] = [0, 0, 0, 0];
    for (let i = 0; i < n; i++) {
      cx += v.vertices[3 * i] / n;
      cy += v.vertices[3 * i + 1] / n;
      cz += v.vertices[3 * i + 2] / n;
    }
    for (let i = 0; i < n; i++) {
      r = Math.max(r, Math.hypot(v.vertices[3 * i] - cx, v.vertices[3 * i + 1] - cy, v.vertices[3 * i + 2] - cz));
    }
    const draw = () => {
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const w = (canvas.width = canvas.clientWidth * devicePixelRatio);
      const h = (canvas.height = canvas.clientHeight * devicePixelRatio);
      const s = (Math.min(w, h) / 2.2) / (r || 1);
      const [cyw, syw, cp, sp] = [Math.cos(yaw), Math.sin(yaw), Math.cos(pitch), Math.sin(pitch)];
      const project = (i: number) => {
        const x = v.vertices[3 * i] - cx;
        const y = v.vertices[3 * i + 1] - cy;
        const z = v.vertices[3 * i + 2] - cz;
        const x1 = x * cyw - y * syw;
        const y1 = x * syw + y * cyw;
        return [w / 2 + x1 * s, h / 2 - (z * cp - y1 * sp) * s];
      };
      ctx.clearRect(0, 0, w, h);
      ctx.strokeStyle = getComputedStyle(canvas).color;
      ctx.lineWidth = 0.5 * devicePixelRatio;
      ctx.beginPath();
      const limit = Math.min(v.triangles.length / 3, 60_000);
      for (let t = 0; t < limit; t++) {
        const [a, b, c] = [project(v.triangles[3 * t]), project(v.triangles[3 * t + 1]), project(v.triangles[3 * t + 2])];
        ctx.moveTo(a[0], a[1]);
        ctx.lineTo(b[0], b[1]);
        ctx.lineTo(c[0], c[1]);
        ctx.closePath();
      }
      ctx.stroke();
    };
    draw();
    let drag: [number, number] | null = null;
    canvas.onpointerdown = (e) => {
      drag = [e.clientX, e.clientY];
      canvas.setPointerCapture(e.pointerId);
    };
    canvas.onpointermove = (e) => {
      if (!drag) return;
      yaw += (e.clientX - drag[0]) / 150;
      pitch = Math.max(-1.5, Math.min(1.5, pitch + (e.clientY - drag[1]) / 150));
      drag = [e.clientX, e.clientY];
      draw();
    };
    canvas.onpointerup = () => (drag = null);
  }

  const PHASE_COLORS = ["var(--syn-number)", "var(--syn-string)", "var(--syn-function)", "var(--syn-property)", "var(--syn-tag)", "var(--syn-keyword)"];
</script>

<div class="overview">
  {#if failure}
    <p class="message error">{failure}</p>
  {:else if !data}
    <p class="message">Reading the standard…</p>
  {:else}
    <article>
      <header>
        <span class="family">{data.family}</span>
        <h1>{data.kind}</h1>
        {#if data.version}<p class="version">{data.version}</p>{/if}
        <p class="tally">
          {#each ["passed", "partial", "failed", "notDone"] as st (st)}
            {@const n = data.checks.filter((c) => c.state === st).length}
            {#if n > 0}<span class={st}><span class="mark">{STATE[st][0]}</span>{n} {STATE[st][1].toLowerCase()}</span>{/if}
          {/each}
        </p>
      </header>

      <section class="panel">
        <h2>What was checked</h2>
        <ul class="checks">
          {#each data.checks as c, i (i)}
            <li class={c.state}>
              <span class="mark" aria-label={STATE[c.state][1]}>{STATE[c.state][0]}</span>
              <span class="stage">{c.stage}</span>
              {#if c.detail}<span class="detail">{c.detail}</span>{/if}
            </li>
          {/each}
        </ul>
      </section>

      {#each groups as [group, facts] (group)}
        <section class="panel">
          <h2>{group}</h2>
          <dl>
            {#each facts as f, i (i)}
              <dt>{f.label}</dt>
              <dd>
                {#if linked(f.source)}
                  <button class="link" onclick={() => onsource?.(f.source!)} title="Show in the text">{f.value}</button>
                {:else}{f.value}{/if}
              </dd>
            {/each}
          </dl>
        </section>
      {/each}

      {#each data.visuals as v, vi (vi)}
        <section class="visual">
          {#if v.kind === "map"}
            {@const all = v.features.flatMap((f) => f.parts.flat())}
            {@const f = fit(all, 900, 520, v.geographic)}
            <h2>Map <span class="muted">{v.features.length} features{v.geographic ? ", longitude and latitude" : ""}; no background is loaded</span></h2>
            <svg viewBox="0 0 900 520" class="map" role="img" aria-label="Map of the features">
              {#each v.features as feat, fi (fi)}
                <g><title>{feat.name}</title>
                  {#each feat.parts as part, pi (pi)}
                    {#if feat.shape === "points"}
                      {#each part as p, k (k)}{@const [x, y] = f(p)}<circle cx={x} cy={y} r="3" />{/each}
                    {:else if feat.shape === "lines"}
                      <polyline points={poly(part, f)} />
                    {:else}
                      <polygon points={poly(part, f)} />
                    {/if}
                  {/each}
                </g>
              {/each}
            </svg>
          {:else if v.kind === "plot"}
            <h2>Signals <span class="muted">{v.channels.length} channels</span></h2>
            {#each v.channels.slice(0, 32) as ch, ci (ci)}
              {@const xs = ch.points.map((p) => p[0])}
              {@const ys = ch.points.map((p) => p[1])}
              {@const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)]}
              <div class="channel">
                <span class="cname">{ch.name}{ch.unit ? ` (${ch.unit})` : ""}</span>
                <svg viewBox="0 0 800 60" preserveAspectRatio="none" role="img" aria-label={`Signal ${ch.name}`}>
                  <polyline
                    vector-effect="non-scaling-stroke"
                    points={ch.points
                      .map((p) => `${(((p[0] - x0) / (x1 - x0 || 1)) * 800).toFixed(1)},${(58 - ((p[1] - y0) / (y1 - y0 || 1)) * 56).toFixed(1)}`)
                      .join(" ")}
                  />
                </svg>
              </div>
            {/each}
          {:else if v.kind === "drawing"}
            {@const all = v.paths.flat() as [number, number][]}
            {@const f = fit(all, 900, 520, false)}
            <h2>Drawing <span class="muted">{v.paths.length} paths, in {v.unit}</span></h2>
            <svg viewBox="0 0 900 520" class="drawing" role="img" aria-label="Drawing">
              {#each v.paths as p, pi (pi)}<polyline points={poly(p as [number, number][], f)} />{/each}
            </svg>
          {:else if v.kind === "picture"}
            <h2>{v.caption || "Picture"}</h2>
            <img src={v.uri} alt={v.caption} />
          {:else if v.kind === "image"}
            <h2>{v.caption || "Image"} <span class="muted">{v.width} × {v.height}</span></h2>
            <canvas class="image" use:drawImage={v}></canvas>
          {:else if v.kind === "timeline"}
            {@const end = Math.max(1, ...v.rows.map((r) => r.start + r.parts.reduce((a, b) => a + b, 0)))}
            <h2>Timeline <span class="muted">{v.rows.length} rows, in {v.unit}</span></h2>
            <div class="legend">
              {#each v.phases as ph, i (ph)}<span><i style:background={PHASE_COLORS[i % PHASE_COLORS.length]}></i>{ph}</span>{/each}
            </div>
            <div class="timeline">
              {#each v.rows.slice(0, 500) as r, ri (ri)}
                <div class="trow" title={r.label}>
                  <span class="tlabel">{r.label}</span>
                  <span class="tbar">
                    {#each r.parts as len, k (k)}
                      {@const before = r.start + r.parts.slice(0, k).reduce((a, b) => a + b, 0)}
                      <i style:left="{(before / end) * 100}%" style:width="{(len / end) * 100}%" style:background={PHASE_COLORS[k % PHASE_COLORS.length]}></i>
                    {/each}
                  </span>
                </div>
              {/each}
            </div>
          {:else if v.kind === "mesh"}
            <h2>3D <span class="muted">{v.total.toLocaleString()} triangles{v.shown < v.total ? `, ${v.shown.toLocaleString()} shown` : ""}; drag to turn</span></h2>
            <canvas class="mesh" use:drawMesh={v}></canvas>
          {:else}
            <p class="muted">{v.text}</p>
          {/if}
        </section>
      {/each}

      {#each data.tables as t, ti (ti)}
        {@const limit = shownRows[ti] ?? 200}
        <section>
          <h2>{t.name} <span class="muted">{t.total.toLocaleString()} rows{t.rows.length < t.total ? `, the first ${t.rows.length.toLocaleString()} listed` : ""}</span></h2>
          <div class="table-wrap">
            <table>
              <thead><tr>{#each t.columns as c, ci (ci)}<th>{c}</th>{/each}</tr></thead>
              <tbody>
                {#each t.rows.slice(0, limit) as r, ri (ri)}
                  <tr
                    class:linked={linked(r.source)}
                    onclick={() => linked(r.source) && onsource?.(r.source!)}
                    title={linked(r.source) ? "Show in the text" : undefined}
                  >
                    {#each r.cells as cell, ci (ci)}<td>{cell}</td>{/each}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
          {#if t.rows.length > limit}
            <button class="more" onclick={() => (shownRows[ti] = limit + 500)}>Show 500 more</button>
          {/if}
        </section>
      {/each}

      {#if data.outline.length > 0}
        <section>
          <h2>Outline <span class="muted">{data.outlineTotal.toLocaleString()} entries</span></h2>
          <ul class="outline">
            {#each data.outline.slice(0, 2000) as o, oi (oi)}
              <li style:padding-left="{o.depth * 16}px">
                {#if linked(o.source)}
                  <button class="link" onclick={() => onsource?.(o.source!)}>{o.label}</button>
                {:else}<span>{o.label}</span>{/if}
                {#if o.value}<span class="muted"> {o.value}</span>{/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if data.notes.length > 0}
        <section>
          <h2>Notes</h2>
          <ul class="notes">{#each data.notes as n, ni (ni)}<li>{n}</li>{/each}</ul>
        </section>
      {/if}
    </article>
  {/if}
</div>

<style>
  .overview {
    height: 100%;
    overflow: auto;
  }
  article {
    max-width: 1000px;
    margin: 0 auto;
    padding: 20px 28px 120px;
  }
  header {
    margin-bottom: 18px;
  }
  .family {
    display: inline-block;
    padding: 1px 9px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 11.5px;
    font-weight: 600;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.01em;
    margin: 8px 0 2px;
  }
  .version {
    margin: 0;
    color: var(--fg-muted);
  }
  .tally {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 10px 0 0;
    font-size: 12px;
  }
  .tally > span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px 2px 3px;
    border-radius: 999px;
    background: var(--bar);
    border: 1px solid var(--line);
    color: var(--fg-muted);
  }
  section {
    margin-top: 16px;
  }
  .panel {
    padding: 12px 16px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bar);
  }
  h2 {
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin: 0 0 10px;
  }
  h2 .muted {
    text-transform: none;
    letter-spacing: 0;
    font-weight: normal;
  }
  .muted {
    color: var(--fg-muted);
  }
  .message {
    padding: 24px;
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .checks li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 6px 0;
    border-top: 1px solid var(--line);
  }
  .checks li:first-child {
    border-top: none;
    padding-top: 0;
  }
  .mark {
    flex: none;
    display: inline-grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    font-size: 11px;
    font-weight: 700;
    line-height: 1;
    background: var(--chip);
    color: var(--fg-muted);
    align-self: center;
  }
  .passed .mark {
    background: var(--ok-bg);
    color: var(--ok);
  }
  .partial .mark {
    background: var(--warn-bg);
    color: var(--warn);
  }
  .failed .mark {
    background: var(--danger-bg);
    color: var(--danger);
  }
  .notDone .stage {
    color: var(--fg-muted);
  }
  .stage {
    flex: none;
    width: 170px;
    font-weight: 600;
  }
  .detail {
    color: var(--fg-muted);
  }
  dl {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 6px 20px;
    margin: 0;
  }
  dt {
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  @media (max-width: 640px) {
    dl {
      grid-template-columns: 1fr;
      gap: 2px;
    }
    dd {
      margin-bottom: 6px;
    }
    .checks li {
      flex-wrap: wrap;
    }
    .stage {
      width: auto;
    }
  }
  button.link {
    font: inherit;
    border: none;
    background: none;
    padding: 0;
    color: inherit;
    text-align: left;
    cursor: pointer;
    border-radius: 3px;
  }
  button.link:hover {
    color: var(--accent);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .table-wrap {
    overflow: auto;
    max-height: 520px;
    border: 1px solid var(--line);
    border-radius: 6px;
  }
  table {
    border-collapse: collapse;
    font-size: 12.5px;
    width: max-content;
    min-width: 100%;
  }
  th,
  td {
    padding: 4px 10px;
    border-bottom: 1px solid var(--line);
    text-align: left;
    white-space: nowrap;
    max-width: 420px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  th {
    position: sticky;
    top: 0;
    background: var(--bar);
  }
  tr.linked {
    cursor: pointer;
  }
  tr.linked:hover td {
    background: var(--active-line);
  }
  .more {
    margin-top: 6px;
    font: inherit;
    border: none;
    background: none;
    color: var(--accent);
    cursor: pointer;
  }
  .outline {
    list-style: none;
    margin: 0;
    padding: 0;
    font-size: 13px;
  }
  .notes {
    color: var(--fg-muted);
  }
  svg.map,
  svg.drawing {
    width: 100%;
    height: auto;
    background: var(--bar);
    border-radius: 6px;
  }
  .map polygon {
    fill: var(--node);
    stroke: var(--accent);
    stroke-width: 1;
  }
  .map polyline,
  .drawing polyline {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
  }
  .map circle {
    fill: var(--syn-keyword);
  }
  .channel {
    display: grid;
    grid-template-columns: 160px 1fr;
    align-items: center;
    gap: 10px;
  }
  .channel svg {
    width: 100%;
    height: 60px;
  }
  .channel polyline {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1;
  }
  .cname {
    font-size: 12px;
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .visual img,
  canvas.image {
    max-width: 100%;
    image-rendering: pixelated;
  }
  canvas.mesh {
    width: 100%;
    height: 480px;
    background: var(--bar);
    border-radius: 6px;
    color: var(--accent);
    cursor: grab;
  }
  .legend {
    display: flex;
    gap: 14px;
    font-size: 12px;
    color: var(--fg-muted);
    margin-bottom: 6px;
  }
  .legend i {
    display: inline-block;
    width: 10px;
    height: 10px;
    margin-right: 4px;
    border-radius: 2px;
  }
  .trow {
    display: grid;
    grid-template-columns: 260px 1fr;
    gap: 8px;
    font-size: 12px;
    height: 18px;
    align-items: center;
  }
  .tlabel {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tbar {
    position: relative;
    height: 10px;
  }
  .tbar i {
    position: absolute;
    top: 0;
    bottom: 0;
    min-width: 1px;
  }
</style>
