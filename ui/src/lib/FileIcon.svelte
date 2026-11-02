<script lang="ts" module>
  export type Category = "text" | "code" | "data" | "image" | "document" | "archive" | "binary" | "standard";

  const DATA = ["csv", "tsv", "sqlite", "xlsx", "parquet", "arrow", "avro", "orc", "hdf5", "npy", "npz", "bibtex"];
  const CODE = ["code", "json", "jsonl", "xml", "yaml", "toml", "ini", "msgpack", "protobuf"];
  const IMAGE = ["png", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "psd", "heif", "avif"];
  const DOCUMENT = ["pdf", "docx", "odt", "notebook", "markdown", "tex"];
  const ARCHIVE = ["zip", "tar", "gzip", "zstd", "xz", "bzip2"];

  /// The family a format belongs to, for its icon and colour.
  export function category(format: string, kind: "text" | "binary", standard: string | null = null): Category {
    if (standard) return "standard";
    if (DATA.includes(format)) return "data";
    if (CODE.includes(format)) return "code";
    if (IMAGE.includes(format)) return "image";
    if (DOCUMENT.includes(format)) return "document";
    if (ARCHIVE.includes(format)) return "archive";
    return kind === "text" ? "text" : "binary";
  }
</script>

<script lang="ts">
  // A small file glyph whose inside and colour say what family the file is.
  let { kind, size = 16 }: { kind: Category; size?: number } = $props();
</script>

<svg class="icon {kind}" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
  <path class="sheet" d="M3.5 1.5h6l3 3v10h-9z" />
  <path class="fold" d="M9.5 1.5v3h3" />
  {#if kind === "text"}
    <path class="mark" d="M5.5 7.5h5M5.5 9.5h5M5.5 11.5h3" />
  {:else if kind === "code"}
    <path class="mark" d="M7 7.5 5.5 9.5 7 11.5M9 7.5l1.5 2-1.5 2" />
  {:else if kind === "data"}
    <path class="mark" d="M5.5 7h5v5h-5zM5.5 9.5h5M8 7v5" />
  {:else if kind === "image"}
    <path class="mark" d="m5.5 12 2-2.5 1.5 1.5 1-1 1 2zM9.8 7.3h.01" />
  {:else if kind === "document"}
    <path class="mark" d="M5.5 7h5M5.5 9h5M5.5 11h5M5.5 13h2.5" />
  {:else if kind === "archive"}
    <path class="mark" d="M8 4v1M8 6.5v1M8 9v1M7 11h2v2H7z" />
  {:else if kind === "standard"}
    <path class="mark" d="m5.8 9.6 1.5 1.5 3-3.2" />
  {:else}
    <path class="mark" d="M5.5 7.5h2v4h-2zM9.5 7.5v4" />
  {/if}
</svg>

<style>
  .icon {
    flex: none;
    fill: none;
    stroke-width: 1.1;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .sheet {
    stroke: currentColor;
    fill: color-mix(in srgb, currentColor 12%, transparent);
  }
  .fold {
    stroke: currentColor;
  }
  .mark {
    stroke: currentColor;
    stroke-width: 1.3;
  }
  .text {
    color: #8b949e;
  }
  .code {
    color: #a371f7;
  }
  .data {
    color: #2da44e;
  }
  .image {
    color: #e3833a;
  }
  .document {
    color: #3b82f6;
  }
  .archive {
    color: #b08800;
  }
  .standard {
    color: #0f9e9e;
  }
  .binary {
    color: #8b949e;
  }
</style>
