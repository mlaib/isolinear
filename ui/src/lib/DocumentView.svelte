<script lang="ts">
  // A Word or OpenDocument text file as HTML the reader built (text escaped, no markup from the
  // file). Links never navigate the window.
  import { documentHtml } from "./api";

  let { path, format }: { path: string; format: string } = $props();

  let html = $state<string | null>(null);
  let failure = $state<string | null>(null);

  $effect(() => {
    const p = path;
    html = null;
    failure = null;
    documentHtml(p, format)
      .then((h) => {
        if (p === path) html = h;
      })
      .catch((e) => (failure = String(e)));
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="document" onclick={(e) => (e.target as HTMLElement).closest("a") && e.preventDefault()}>
  {#if failure}
    <p class="message error">{failure}</p>
  {:else if html === null}
    <p class="message">Reading the document…</p>
  {:else}
    <article>{@html html}</article>
  {/if}
</div>

<style>
  .document {
    height: 100%;
    overflow: auto;
  }
  article {
    max-width: 760px;
    margin: 0 auto;
    padding: 28px 32px 120px;
    font-size: 15px;
    line-height: 1.6;
    overflow-wrap: break-word;
  }
  .message {
    padding: 24px;
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
  article :global(table) {
    border-collapse: collapse;
    margin: 1em 0;
  }
  article :global(td),
  article :global(th) {
    border: 1px solid var(--line);
    padding: 4px 10px;
    vertical-align: top;
  }
  article :global(img) {
    max-width: 100%;
  }
  article :global(a) {
    color: var(--syn-link);
  }
  article :global(.note) {
    color: var(--fg-muted);
    font-size: 0.9em;
  }
  article :global(blockquote) {
    margin: 0;
    padding: 0 1em;
    border-left: 4px solid var(--line);
    color: var(--fg-muted);
  }
</style>
