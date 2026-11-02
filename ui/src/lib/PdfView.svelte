<script lang="ts">
  // PDF pages drawn by the Rust renderer as images, each when it scrolls into sight. Nothing in
  // the document runs: there is no JavaScript, form or link handling here.
  import { onMount, untrack } from "svelte";
  import { pdfInfo, pdfPage } from "./api";

  let { path }: { path: string } = $props();

  let pages = $state(0);
  let failure = $state<string | null>(null);
  let zoom = $state(1);
  let width = $state(800);
  let images = $state<(string | null)[]>([]);
  let sizes = $state<({ w: number; h: number } | null)[]>([]);
  let errors = $state<(string | null)[]>([]);
  let stage: HTMLDivElement;
  const loading = new Set<number>();
  // Pages in or near sight, and the zoom generation a drawing must belong to to be kept.
  const seen = new Set<number>();
  let generation = 0;

  // Points across a US Letter page; pages are drawn to fill the width at zoom 1.
  const PAGE_POINTS = 612;
  const scale = $derived(Math.min(6, Math.max(0.25, ((width - 48) / PAGE_POINTS) * zoom * (window.devicePixelRatio || 1))));

  $effect(() => {
    const p = path;
    pages = 0;
    failure = null;
    pdfInfo(p)
      .then((info) => {
        if (p !== path) return;
        pages = info.pages;
        images = Array(info.pages).fill(null);
        sizes = Array(info.pages).fill(null);
        errors = Array(info.pages).fill(null);
      })
      .catch((e) => (failure = String(e)));
  });

  function draw(i: number) {
    if (loading.has(i) || images[i]) return;
    loading.add(i);
    const g = generation;
    pdfPage(path, i, scale)
      .then((uri) => g === generation && (images[i] = uri))
      .catch((e) => g === generation && (errors[i] = String(e)))
      .finally(() => {
        loading.delete(i);
        if (g !== generation && seen.has(i)) draw(i);
      });
  }

  // A new zoom drops every drawing and draws the pages in sight again; the others are drawn
  // when they are next seen. Only the scale is tracked: drawing a page must not clear it.
  $effect(() => {
    void scale;
    untrack(() => {
      generation++;
      images = images.map(() => null);
      errors = errors.map(() => null);
      for (const i of seen) draw(i);
    });
  });

  function visible(node: HTMLElement, i: number) {
    const io = new IntersectionObserver(
      (entries) => {
        const e = entries[entries.length - 1];
        if (e.isIntersecting) {
          seen.add(i);
          draw(i);
        } else seen.delete(i);
      },
      { root: stage, rootMargin: "600px 0px" },
    );
    io.observe(node);
    return {
      destroy: () => {
        seen.delete(i);
        io.disconnect();
      },
    };
  }

  onMount(() => {
    const ro = new ResizeObserver(() => (width = stage.clientWidth));
    ro.observe(stage);
    return () => ro.disconnect();
  });
</script>

<div class="pdf">
  <div class="bar">
    <button onclick={() => (zoom = Math.max(0.25, zoom / 1.25))} aria-label="Zoom out">−</button>
    <span>{Math.round(zoom * 100)}%</span>
    <button onclick={() => (zoom = Math.min(4, zoom * 1.25))} aria-label="Zoom in">+</button>
    <button onclick={() => (zoom = 1)}>Fit width</button>
    {#if pages}<span class="muted">{pages} {pages === 1 ? "page" : "pages"}</span>{/if}
  </div>
  <div class="stage" bind:this={stage}>
    {#if failure}
      <p class="error">{failure}</p>
    {:else}
      {#each images as img, i (i)}
        <div
          class="page"
          use:visible={i}
          style:width="{((width - 48) * zoom)}px"
          style:aspect-ratio={sizes[i] ? `${sizes[i]!.w} / ${sizes[i]!.h}` : "612 / 792"}
        >
          {#if img}
            <img
              src={img}
              alt="Page {i + 1}"
              onload={(e) => {
                const el = e.currentTarget as HTMLImageElement;
                sizes[i] = { w: el.naturalWidth, h: el.naturalHeight };
              }}
            />
          {:else if errors[i]}
            <p class="error">Page {i + 1}: {errors[i]}</p>
          {:else}
            <p class="muted">Page {i + 1}</p>
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</div>

<style>
  .pdf {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
    font-size: 12.5px;
  }
  .bar button {
    font: inherit;
    padding: 1px 9px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .stage {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 16px 24px;
    background: var(--chip);
  }
  .page {
    margin: 0 auto 16px;
    background: #fff;
    box-shadow: 0 1px 4px rgb(0 0 0 / 0.3);
    display: grid;
    place-items: center;
    max-width: none;
  }
  .page img {
    width: 100%;
    height: 100%;
    display: block;
  }
  .muted {
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
    padding: 12px;
  }
</style>
