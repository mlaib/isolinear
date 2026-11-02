<script lang="ts">
  // A Jupyter notebook as cells: code highlighted with its execution count, Markdown rendered,
  // and the outputs the file saved. Nothing runs: HTML and JavaScript outputs are shown as
  // source, widgets are named.
  import { enhance, highlightInto } from "./enhance";
  import { readNotebook, type Notebook } from "./api";

  let { path }: { path: string } = $props();

  let nb = $state<Notebook | null>(null);
  let failure = $state<string | null>(null);

  $effect(() => {
    const p = path;
    nb = null;
    failure = null;
    readNotebook(p)
      .then((n) => {
        if (p === path) nb = n;
      })
      .catch((e) => (failure = String(e)));
  });

  function highlight(node: HTMLElement, language: string | null) {
    highlightInto(node, language);
  }

  const MIME_NAMES: Record<string, string> = {
    "text/html": "HTML output, shown as source",
    "application/javascript": "JavaScript output, not run",
    "text/latex": "LaTeX output",
    "application/json": "JSON output",
  };
</script>

<div class="notebook">
  {#if failure}
    <p class="message error">{failure}</p>
  {:else if !nb}
    <p class="message">Reading the notebook…</p>
  {:else}
    <article>
      <p class="meta">
        {nb.kernel ?? nb.language ?? "Notebook"} · {nb.cells.length} cells · format {nb.nbformat} · outputs as saved, nothing
        is run
      </p>
      {#each nb.cells as cell, i (i)}
        <section class="cell {cell.kind}">
          {#if cell.kind === "code"}
            <div class="prompt">In [{cell.executionCount ?? " "}]:</div>
            <div class="body">
              <pre class="input"><code use:highlight={nb.language}>{cell.source}</code></pre>
              {#each cell.outputs as out, j (j)}
                <div class="output">
                  {#if out.kind === "stream"}
                    <pre class:stderr={out.name === "stderr"}>{out.text}</pre>
                  {:else if out.kind === "text"}
                    <pre>{out.text}</pre>
                  {:else if out.kind === "image"}
                    <img src={out.uri} alt={out.alt} />
                  {:else if out.kind === "html"}
                    <div class="rendered" use:enhance>{@html out.html}</div>
                  {:else if out.kind === "source"}
                    <p class="label">{MIME_NAMES[out.mime] ?? out.mime}</p>
                    <pre class="source">{out.text}</pre>
                  {:else if out.kind === "error"}
                    <pre class="error-output"><strong>{out.name}: {out.value}</strong>{"\n"}{out.traceback}</pre>
                  {:else}
                    <p class="label">Not shown: {out.what}.</p>
                  {/if}
                </div>
              {/each}
            </div>
          {:else if cell.kind === "markdown"}
            <div class="prompt"></div>
            <div class="body rendered" use:enhance>{@html cell.html}</div>
          {:else}
            <div class="prompt"></div>
            <pre class="body raw">{cell.source}</pre>
          {/if}
        </section>
      {/each}
    </article>
  {/if}
</div>

<style>
  .notebook {
    height: 100%;
    overflow: auto;
  }
  article {
    max-width: 980px;
    margin: 0 auto;
    padding: 16px 24px 120px;
  }
  .meta {
    color: var(--fg-muted);
    font-size: 12.5px;
    margin: 0 0 16px;
  }
  .message {
    padding: 24px;
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
  .cell {
    display: grid;
    grid-template-columns: 76px 1fr;
    gap: 8px;
    margin-bottom: 14px;
  }
  .prompt {
    font-family: var(--mono);
    font-size: 12px;
    color: var(--fg-muted);
    text-align: right;
    padding-top: 8px;
  }
  .body {
    min-width: 0;
  }
  pre {
    margin: 0;
    font-family: var(--mono);
    font-size: 12.5px;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .input {
    background: var(--bar);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 8px 12px;
  }
  .output {
    padding: 6px 12px 0;
    max-height: 480px;
    overflow: auto;
  }
  .output img {
    max-width: 100%;
    background: #fff;
  }
  .stderr {
    background: var(--danger-bg);
    padding: 4px 8px;
    border-radius: 4px;
  }
  .error-output {
    color: var(--danger);
    background: var(--danger-bg);
    padding: 6px 8px;
    border-radius: 4px;
  }
  .label {
    margin: 0 0 4px;
    color: var(--fg-muted);
    font-size: 12px;
    font-style: italic;
  }
  .source {
    color: var(--fg-muted);
  }
  .raw {
    color: var(--fg-muted);
  }
  .rendered {
    font-size: 14.5px;
    line-height: 1.6;
    overflow-wrap: break-word;
  }
  .rendered :global(h1),
  .rendered :global(h2) {
    border-bottom: 1px solid var(--line);
    padding-bottom: 0.25em;
  }
  .rendered :global(code) {
    font-family: var(--mono);
    font-size: 0.88em;
    background: var(--chip);
    border-radius: 4px;
    padding: 0.1em 0.35em;
  }
  .rendered :global(pre) {
    background: var(--bar);
    padding: 8px 12px;
    border-radius: 6px;
  }
  .rendered :global(pre code) {
    background: none;
    padding: 0;
  }
  .rendered :global(img) {
    max-width: 100%;
  }
  .rendered :global(table) {
    border-collapse: collapse;
  }
  .rendered :global(th),
  .rendered :global(td) {
    border: 1px solid var(--line);
    padding: 4px 10px;
  }
  .rendered :global(.raw-html) {
    font-family: var(--mono);
    font-size: 0.85em;
    color: var(--fg-muted);
  }
  :global(.tok-keyword) {
    color: var(--syn-keyword);
  }
  :global(.tok-string),
  :global(.tok-string2) {
    color: var(--syn-string);
  }
  :global(.tok-number),
  :global(.tok-bool),
  :global(.tok-atom) {
    color: var(--syn-number);
  }
  :global(.tok-comment) {
    color: var(--syn-comment);
    font-style: italic;
  }
  :global(.tok-propertyName) {
    color: var(--syn-property);
  }
  :global(.tok-typeName),
  :global(.tok-className) {
    color: var(--syn-type);
  }
  :global(.tok-definition),
  :global(.tok-function) {
    color: var(--syn-function);
  }
</style>
