<script lang="ts">
  // What a file is: its type and why, its size, and the facts its header gives.
  import { fileSummary, type DocumentInfo } from "./api";

  let { doc }: { doc: DocumentInfo } = $props();

  let facts = $state<[string, string][] | null>(null);
  let failure = $state<string | null>(null);

  $effect(() => {
    const path = doc.path;
    facts = null;
    fileSummary(path)
      .then((s) => {
        if (path === doc.path) facts = s.facts;
      })
      .catch((e) => (failure = String(e)));
  });

  const exactSize = (n: number) => `${n.toLocaleString()} bytes`;
</script>

<div class="summary">
  <article>
    <h1>{doc.name}</h1>
    <dl>
      <dt>Type</dt>
      <dd>{doc.format.name}</dd>
      <dt>Recognized</dt>
      <dd>{doc.reason}</dd>
      <dt>Size</dt>
      <dd>{exactSize(doc.size)}</dd>
      <dt>Location</dt>
      <dd class="path">{doc.path}</dd>
      {#each facts ?? [] as [k, v] (k)}
        <dt>{k}</dt>
        <dd class:mono={k === "First bytes"}>{v}</dd>
      {/each}
    </dl>
    {#if failure}<p class="error">{failure}</p>{/if}
    <p class="note">Read-only. Nothing in the file is run or loaded.</p>
  </article>
</div>

<style>
  .summary {
    height: 100%;
    overflow: auto;
  }
  article {
    max-width: 820px;
    margin: 0 auto;
    padding: 24px 28px 48px;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.01em;
    margin: 0 0 16px;
    word-break: break-all;
  }
  dl {
    display: grid;
    grid-template-columns: 160px 1fr;
    gap: 8px 20px;
    margin: 0;
    padding: 14px 16px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bar);
  }
  @media (max-width: 600px) {
    dl {
      grid-template-columns: 1fr;
      gap: 2px;
    }
  }
  dt {
    color: var(--fg-muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .mono,
  .path {
    font-family: var(--mono);
    font-size: 12.5px;
  }
  .note {
    margin-top: 24px;
    color: var(--fg-muted);
    font-size: 12.5px;
  }
  .error {
    color: var(--danger);
  }
</style>
