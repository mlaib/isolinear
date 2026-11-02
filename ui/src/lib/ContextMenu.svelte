<script lang="ts">
  // The window's own right-click menu, in place of the web view's (whose Reload and Back would
  // drop the open document). Closes on a choice, Escape, a click elsewhere or a scroll.
  import { onMount, tick } from "svelte";

  export type MenuItem =
    | { label: string; hint?: string; disabled?: boolean; checked?: boolean; run: () => void }
    | "separator";

  let { x, y, items, onclose }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  let menu: HTMLDivElement;
  let left = $state(0);
  let top = $state(0);
  let placed = $state(false);

  onMount(() => {
    // Keep the menu inside the window.
    const r = menu.getBoundingClientRect();
    left = Math.max(4, Math.min(x, window.innerWidth - r.width - 4));
    top = Math.max(4, Math.min(y, window.innerHeight - r.height - 4));
    placed = true;
    tick().then(() => menu.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus());
    const away = (e: Event) => {
      if (!menu.contains(e.target as Node)) onclose();
    };
    window.addEventListener("pointerdown", away, true);
    window.addEventListener("wheel", onclose, true);
    window.addEventListener("blur", onclose);
    return () => {
      window.removeEventListener("pointerdown", away, true);
      window.removeEventListener("wheel", onclose, true);
      window.removeEventListener("blur", onclose);
    };
  });

  function onKey(e: KeyboardEvent) {
    const buttons = [...menu.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next = (i + (e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
      buttons[next]?.focus();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="menu" role="menu" tabindex="-1" bind:this={menu} style:left="{left}px" style:top="{top}px" style:visibility={placed ? "visible" : "hidden"} onkeydown={onKey}>
  {#each items as item, i (i)}
    {#if item === "separator"}
      <div class="separator" role="separator"></div>
    {:else}
      <button
        role={item.checked === undefined ? "menuitem" : "menuitemradio"}
        aria-checked={item.checked}
        disabled={item.disabled}
        onclick={() => {
          onclose();
          item.run();
        }}
      >
        <span class="label">
          {#if items.some((i) => i !== "separator" && i.checked !== undefined)}
            <span class="check" aria-hidden="true">{item.checked ? "✓" : ""}</span>
          {/if}
          {item.label}
        </span>
        {#if item.hint}<kbd>{item.hint}</kbd>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 100;
    min-width: 200px;
    padding: 4px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow:
      0 8px 24px rgb(0 0 0 / 0.18),
      0 1px 3px rgb(0 0 0 / 0.12);
    font-size: 13px;
  }
  button {
    display: flex;
    width: 100%;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 5px 10px;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: default;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: var(--accent);
    color: #fff;
    outline: none;
  }
  button:hover:not(:disabled) kbd,
  button:focus-visible kbd {
    color: rgb(255 255 255 / 0.8);
  }
  button:disabled {
    color: var(--fg-muted);
    opacity: 0.6;
  }
  .label {
    display: flex;
    align-items: center;
  }
  .check {
    width: 18px;
  }
  kbd {
    font: inherit;
    font-size: 12px;
    color: var(--fg-muted);
  }
  .separator {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
</style>
