<script lang="ts">
  // The start window: open a file, reopen a recent one, and what the app does.
  import FileIcon, { category } from "./FileIcon.svelte";
  import Logo from "./Logo.svelte";

  export type Recent = { path: string; name: string; format: string; kind: "text" | "binary" };

  let {
    recents,
    onchoose,
    onopen,
    onforget,
  }: {
    recents: Recent[];
    onchoose: () => void;
    onopen: (path: string) => void;
    onforget: (path: string) => void;
  } = $props();

  const folder = (path: string) => {
    const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    return i > 0 ? path.slice(0, i) : path;
  };

  const FEATURES = [
    {
      kind: "code" as const,
      title: "Edit structured text",
      text: "Markdown, TeX, JSON, XML, YAML, TOML, CSV and code, with the structure beside the text.",
    },
    {
      kind: "data" as const,
      title: "Explore data",
      text: "Parquet, SQLite, Excel, Avro, ORC, HDF5 and NumPy as tables, read page by page.",
    },
    {
      kind: "document" as const,
      title: "Read documents",
      text: "PDF, Word, notebooks, images and archives, previewed in place.",
    },
    {
      kind: "standard" as const,
      title: "Understand standards",
      text: "ISO 20022, FHIR, EDIFACT, DICOM, IFC, GeoJSON and many more, checked and explained.",
    },
  ];
</script>

<div class="welcome">
  <div class="hero">
    <Logo size={72} />
    <h1>Isolinear</h1>
    <p class="tagline">Open any text, data or industry file. Nothing in it is run, nothing is fetched.</p>
    <div class="actions">
      <button class="primary" onclick={onchoose}>Open a file…</button>
      <span class="hint"><kbd>Ctrl</kbd> <kbd>O</kbd> or drop files anywhere</span>
    </div>
  </div>

  {#if recents.length > 0}
    <section class="recent" aria-label="Recent files">
      <h2>Recent</h2>
      <ul>
        {#each recents as r (r.path)}
          <li>
            <button class="file" onclick={() => onopen(r.path)} title={r.path}>
              <FileIcon kind={category(r.format, r.kind)} />
              <span class="name">{r.name}</span>
              <span class="where">{folder(r.path)}</span>
            </button>
            <button class="forget" onclick={() => onforget(r.path)} title="Remove from the list" aria-label="Remove {r.name} from the list"
              >×</button
            >
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section class="features" aria-label="What Isolinear does">
    {#each FEATURES as f (f.title)}
      <div class="card">
        <FileIcon kind={f.kind} size={20} />
        <h3>{f.title}</h3>
        <p>{f.text}</p>
      </div>
    {/each}
  </section>

  <p class="foot">Press <kbd>F1</kbd> for the keyboard shortcuts · <code>isolinear FILE</code> opens files from a terminal</p>
</div>

<style>
  .welcome {
    height: 100%;
    overflow: auto;
    padding: 9vh 24px 32px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 36px;
  }
  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
  }
  h1 {
    font-size: 28px;
    font-weight: 650;
    letter-spacing: -0.02em;
    margin: 16px 0 6px;
  }
  .tagline {
    margin: 0;
    color: var(--fg-muted);
    font-size: 15px;
    max-width: 540px;
  }
  .actions {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    margin-top: 22px;
  }
  button.primary {
    font: inherit;
    font-size: 14px;
    font-weight: 600;
    padding: 8px 22px;
    border: 1px solid var(--accent);
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    cursor: pointer;
    box-shadow: var(--shadow-sm);
  }
  button.primary:hover {
    filter: brightness(1.08);
  }
  .hint,
  .foot {
    color: var(--fg-muted);
    font-size: 12.5px;
  }
  kbd {
    font: inherit;
    font-size: 11.5px;
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-bottom-width: 2px;
    border-radius: 4px;
    background: var(--bar);
  }
  .recent {
    width: min(560px, 100%);
  }
  h2 {
    font-size: 11.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
    margin: 0 0 6px 10px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    border-radius: 7px;
  }
  li:hover {
    background: var(--hover);
  }
  .file {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: none;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 13.5px;
    text-align: left;
    cursor: pointer;
  }
  .name {
    font-weight: 500;
    white-space: nowrap;
  }
  .where {
    color: var(--fg-muted);
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .forget {
    visibility: hidden;
    border: none;
    background: none;
    color: var(--fg-muted);
    font-size: 16px;
    padding: 2px 10px;
    cursor: pointer;
  }
  li:hover .forget,
  .forget:focus-visible {
    visibility: visible;
  }
  .features {
    width: min(760px, 100%);
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 12px;
  }
  .card {
    padding: 14px 14px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--bar);
  }
  h3 {
    font-size: 13.5px;
    margin: 8px 0 4px;
  }
  .card p {
    margin: 0;
    color: var(--fg-muted);
    font-size: 12.5px;
    line-height: 1.45;
  }
  .foot {
    margin: 0;
  }
  code {
    font-size: 12px;
  }
</style>
