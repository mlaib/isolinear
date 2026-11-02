<script lang="ts">
  // Rendered Markdown, one element per source block. Unchanged blocks keep their DOM when the
  // text changes. Math goes through KaTeX and code blocks through the editor's language parsers.
  // Links never navigate the window; links to anchors in the document scroll to them.
  import { tick } from "svelte";
  import { enhance } from "./enhance";
  import type { PreviewBlock } from "./api";

  let {
    blocks,
    active = -1,
    onpick,
  }: {
    blocks: PreviewBlock[];
    /// Index of the block under the editor's cursor.
    active?: number;
    onpick?: (block: PreviewBlock) => void;
  } = $props();

  let root: HTMLDivElement;

  // Keys from content, so an edit re-renders only the blocks it changed.
  const keyed = $derived.by(() => {
    const seen = new Map<string, number>();
    return blocks.map((b) => {
      const n = (seen.get(b.html) ?? 0) + 1;
      seen.set(b.html, n);
      return { block: b, key: `${n}:${b.html}` };
    });
  });

  function onClick(e: MouseEvent) {
    const link = (e.target as HTMLElement).closest("a");
    if (link) {
      e.preventDefault();
      const href = link.getAttribute("href") ?? "";
      if (href.startsWith("#")) {
        const id = decodeURIComponent(href.slice(1));
        root.querySelector(`[id="${CSS.escape(id)}"]`)?.scrollIntoView({ block: "start" });
      }
      return;
    }
    const el = (e.target as HTMLElement).closest<HTMLElement>("[data-block]");
    if (el && window.getSelection()?.isCollapsed !== false) onpick?.(blocks[Number(el.dataset.block)]);
  }

  // Keep the active block in view.
  $effect(() => {
    const i = active;
    if (i < 0) return;
    tick().then(() => {
      const el = root?.querySelector<HTMLElement>(`[data-block="${i}"]`);
      if (!el) return;
      const r = el.getBoundingClientRect();
      const box = root.getBoundingClientRect();
      if (r.top < box.top || r.bottom > box.bottom) el.scrollIntoView({ block: "center" });
    });
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="preview" bind:this={root} onclick={onClick}>
  <article>
    {#each keyed as { block, key }, i (key)}
      <div class="block" class:active={i === active} data-block={i} use:enhance>{@html block.html}</div>
    {/each}
    {#if blocks.length === 0}<p class="empty">Nothing to show yet.</p>{/if}
  </article>
</div>

<style>
  .preview {
    height: 100%;
    overflow: auto;
    background: var(--bg);
  }
  article {
    max-width: 760px;
    margin: 0 auto;
    padding: 24px 32px 120px;
    font-size: 15px;
    line-height: 1.6;
    overflow-wrap: break-word;
  }
  .block {
    border-left: 2px solid transparent;
    margin-left: -14px;
    padding-left: 12px;
  }
  .block.active {
    border-left-color: var(--accent);
  }
  .empty {
    color: var(--fg-muted);
  }
  article :global(h1),
  article :global(h2) {
    border-bottom: 1px solid var(--line);
    padding-bottom: 0.3em;
  }
  article :global(h1) {
    font-size: 1.9em;
    margin: 0.6em 0 0.5em;
  }
  article :global(h2) {
    font-size: 1.45em;
    margin: 1.2em 0 0.5em;
  }
  article :global(h3) {
    font-size: 1.2em;
    margin: 1.1em 0 0.4em;
  }
  article :global(a) {
    color: var(--syn-link);
    text-decoration: none;
  }
  article :global(a:hover) {
    text-decoration: underline;
  }
  article :global(code) {
    font-family: var(--mono);
    font-size: 0.88em;
    background: var(--chip);
    border-radius: 4px;
    padding: 0.1em 0.35em;
  }
  article :global(pre) {
    background: var(--bar);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 12px 14px;
    overflow: auto;
    line-height: 1.45;
  }
  article :global(pre code) {
    background: none;
    padding: 0;
    font-size: 13px;
  }
  article :global(blockquote) {
    margin: 0;
    padding: 0 1em;
    color: var(--fg-muted);
    border-left: 4px solid var(--line);
  }
  article :global(table) {
    border-collapse: collapse;
    display: block;
    overflow: auto;
  }
  article :global(th),
  article :global(td) {
    border: 1px solid var(--line);
    padding: 5px 12px;
  }
  article :global(th) {
    background: var(--bar);
  }
  article :global(img) {
    max-width: 100%;
  }
  article :global(hr) {
    border: none;
    border-top: 1px solid var(--line);
    margin: 1.5em 0;
  }
  article :global(li > input[type="checkbox"]) {
    margin-right: 0.4em;
  }
  article :global(.raw-html) {
    font-family: var(--mono);
    font-size: 0.85em;
    color: var(--fg-muted);
  }
  article :global(.image-placeholder) {
    display: inline-block;
    padding: 2px 8px;
    border: 1px dashed var(--line);
    border-radius: 4px;
    color: var(--fg-muted);
    font-size: 0.9em;
  }
  article :global(.math.display) {
    display: block;
    overflow-x: auto;
  }
  article :global(.tok-keyword) {
    color: var(--syn-keyword);
  }
  article :global(.tok-string),
  article :global(.tok-string2) {
    color: var(--syn-string);
  }
  article :global(.tok-number),
  article :global(.tok-bool),
  article :global(.tok-atom) {
    color: var(--syn-number);
  }
  article :global(.tok-comment) {
    color: var(--syn-comment);
    font-style: italic;
  }
  article :global(.tok-propertyName) {
    color: var(--syn-property);
  }
  article :global(.tok-typeName),
  article :global(.tok-className) {
    color: var(--syn-type);
  }
  article :global(.tok-definition),
  article :global(.tok-function) {
    color: var(--syn-function);
  }
  article :global(.tok-meta) {
    color: var(--syn-comment);
  }
</style>
