<script lang="ts">
  // An image file, fitted to the window or at its actual size, on a checkerboard so transparency
  // shows. The page shows the file's bytes; nothing else is loaded.
  import { imageData } from "./api";

  let { path, format, onsize }: { path: string; format: string; onsize?: (w: number, h: number) => void } = $props();

  let uri = $state<string | null>(null);
  let failure = $state<string | null>(null);
  let fit = $state(true);
  let natural = $state<{ w: number; h: number } | null>(null);

  $effect(() => {
    const p = path;
    uri = null;
    failure = null;
    natural = null;
    imageData(p, format)
      .then((u) => {
        if (p === path) uri = u;
      })
      .catch((e) => (failure = String(e)));
  });

  function loaded(e: Event) {
    const img = e.currentTarget as HTMLImageElement;
    natural = { w: img.naturalWidth, h: img.naturalHeight };
    onsize?.(img.naturalWidth, img.naturalHeight);
  }
</script>

<div class="image">
  <div class="bar">
    <div class="segmented" role="group" aria-label="Zoom">
      <button class:on={fit} onclick={() => (fit = true)}>Fit</button>
      <button class:on={!fit} onclick={() => (fit = false)}>Actual size</button>
    </div>
    {#if natural}<span class="muted">{natural.w} × {natural.h} pixels</span>{/if}
  </div>
  <div class="stage" class:fit>
    {#if failure}
      <p class="error">{failure}</p>
    {:else if uri}
      <img src={uri} alt={path.split(/[\\/]/).pop()} onload={loaded} onerror={() => (failure = "This image could not be decoded.")} />
    {:else}
      <p class="muted">Opening…</p>
    {/if}
  </div>
</div>

<style>
  .image {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
    font-size: 12.5px;
  }
  .segmented {
    display: inline-flex;
    border: 1px solid var(--line);
    border-radius: 6px;
    overflow: hidden;
  }
  .segmented button {
    font: inherit;
    border: none;
    padding: 2px 10px;
    background: transparent;
    color: var(--fg-muted);
  }
  .segmented button.on {
    background: var(--chip);
    color: var(--fg);
    font-weight: 600;
  }
  .stage {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: grid;
    place-items: center;
    background-color: var(--bg);
    background-image: linear-gradient(45deg, var(--chip) 25%, transparent 25%),
      linear-gradient(-45deg, var(--chip) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--chip) 75%),
      linear-gradient(-45deg, transparent 75%, var(--chip) 75%);
    background-size: 20px 20px;
    background-position: 0 0, 0 10px, 10px -10px, -10px 0;
  }
  .stage.fit img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  img {
    image-rendering: auto;
  }
  .muted {
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
</style>
