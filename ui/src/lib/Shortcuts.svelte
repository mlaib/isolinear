<script lang="ts">
  // Keyboard shortcuts, opened with F1. Escape closes it.
  let { onclose }: { onclose: () => void } = $props();

  const mod = navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl";
  const groups: [string, [string, string][]][] = [
    [
      "Files and windows",
      [
        [`${mod}+O`, "Open a file (in this window when it is empty)"],
        [`${mod}+S`, "Save"],
        [`${mod}+E`, "Switch between View and Edit"],
        ["Alt+1 … Alt+9", "Show the first, second … view"],
        ["F1", "These shortcuts"],
      ],
    ],
    [
      "Text",
      [
        [`${mod}+F`, "Find and replace"],
        [`${mod}+G`, "Go to line"],
        [`${mod}+/`, "Comment or uncomment the lines"],
        [`${mod}+Z / ${mod}+Shift+Z`, "Undo / redo"],
        ["Alt+click, Alt+drag", "More cursors, a rectangular selection"],
      ],
    ],
    [
      "Markdown, while editing",
      [
        [`${mod}+B / ${mod}+I`, "Bold / italic"],
        [`${mod}+K / ${mod}+Shift+K`, "Link / image"],
        [`${mod}+1 … ${mod}+6, ${mod}+0`, "Heading level, no heading"],
        [`${mod}+Alt+T`, "Line up the table under the cursor"],
      ],
    ],
    [
      "Trees, tables, large files",
      [
        ["Arrows, Home, End", "Move; Right and Left open and close a node"],
        ["Enter", "Open the cell or node in the text"],
        [`${mod}+C`, "Copy the cell, or the node as written"],
        [`${mod}+F, F3`, "Find in a large file, find next"],
        [`${mod}+G`, "Go to a line, or to an offset in Hex"],
      ],
    ],
  ];

  let dialog: HTMLDivElement;
  $effect(() => dialog?.focus());
</script>

<div class="scrim" role="presentation" onclick={onclose}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="shortcuts-title"
    tabindex="-1"
    bind:this={dialog}
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === "Escape" && (e.preventDefault(), onclose())}
  >
    <h2 id="shortcuts-title">Keyboard shortcuts</h2>
    {#each groups as [title, keys] (title)}
      <h3>{title}</h3>
      <dl>
        {#each keys as [k, what] (k)}<dt><kbd>{k}</kbd></dt><dd>{what}</dd>{/each}
      </dl>
    {/each}
    <div class="actions"><button onclick={onclose}>Close</button></div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.35);
    display: grid;
    place-items: center;
    z-index: 10;
  }
  .dialog {
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 18px 22px;
    width: min(640px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow: auto;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.25);
    outline: none;
  }
  h2 {
    margin: 0 0 6px;
    font-size: 16px;
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-muted);
  }
  dl {
    display: grid;
    grid-template-columns: 230px 1fr;
    gap: 5px 18px;
    margin: 0;
  }
  dd {
    margin: 0;
  }
  kbd {
    font-family: var(--mono);
    font-size: 12px;
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--bar);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
  button {
    font: inherit;
    padding: 3px 12px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
</style>
