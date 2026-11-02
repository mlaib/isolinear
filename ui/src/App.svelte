<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import Editor from "./lib/Editor.svelte";
  import PagedView from "./lib/PagedView.svelte";
  import TreeView from "./lib/TreeView.svelte";
  import TableView from "./lib/TableView.svelte";
  import MarkdownPreview from "./lib/MarkdownPreview.svelte";
  import HexView from "./lib/HexView.svelte";
  import StringsView from "./lib/StringsView.svelte";
  import ContextMenu, { type MenuItem } from "./lib/ContextMenu.svelte";
  import FileIcon, { category } from "./lib/FileIcon.svelte";
  import Welcome, { type Recent } from "./lib/Welcome.svelte";
  import { applyTheme, savedTheme, type Theme } from "./lib/colorScheme";
  import { getVersion } from "@tauri-apps/api/app";
  import { EditorView } from "@codemirror/view";
  import SummaryView from "./lib/SummaryView.svelte";
  import ImageView from "./lib/ImageView.svelte";
  import NotebookView from "./lib/NotebookView.svelte";
  import Shortcuts from "./lib/Shortcuts.svelte";
  import PdfView from "./lib/PdfView.svelte";
  import DocumentView from "./lib/DocumentView.svelte";
  import OverviewView from "./lib/OverviewView.svelte";
  import * as md from "./lib/markdownCommands";
  import type { Indent } from "./lib/indent";
  import type { KeyBinding } from "@codemirror/view";
  import {
    takePaths,
    copyText,
    previewMode,
    previewHide,
    previewSelect,
    onOpenPaths,
    openInNewWindows,
    pickFiles,
    archiveExtract,
    openDocument,
    saveDocument,
    onDiskChange,
    sameStamp,
    structureUpdate,
    onStructure,
    treePath,
    treeDiagnostics,
    formatText,
    formattedFromSource,
    formattedToSource,
    onTable,
    tableUpdate,
    tableCellSpan,
    tableCellAt,
    tableSwitch,
    renderMarkdown,
    type PreviewBlock,
    type OverviewSource,
    type Cell,
    type TableReady,
    type DocumentInfo,
    type IndexProgress,
    type NodeRow,
    type StructureReady,
    type View,
    type SaveError,
    type Stamp,
  } from "./lib/api";

  let doc = $state<DocumentInfo | null>(null);
  let error: string | null = $state(null);
  let loading = $state(false);
  let editor: Editor | undefined = $state();

  // Editing state. `baseline` is the text as last read from or written to disk.
  let mode: "view" | "edit" = $state("view");
  let baseline = $state("");
  let dirty = $state(false);
  let stamp: Stamp | null = $state(null);
  let saving = $state(false);
  let notice: { text: string; kind: "info" | "warn" | "error" } | null = $state(null);
  // Something happened on disk that needs a decision.
  let disk: "changed" | "missing" | "conflict" | null = $state(null);
  let closeAsk = $state(false);
  /// The file manager's quick preview: one window, given each file in turn, hidden on Escape.
  let quickPreview = $state(false);
  let shortcutsOpen = $state(false);
  /// The window's own right-click menu (the web view's would offer Reload).
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  /// A short confirmation shown at the bottom of the window for a moment.
  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  /// Files being dragged over the window.
  let dragging = $state(false);
  let theme = $state<Theme>(savedTheme());
  let aboutOpen = $state(false);
  let version = $state("");
  /// Files opened lately, newest first: a convenience kept in this window's storage only.
  let recents = $state<Recent[]>(loadRecents());

  function loadRecents(): Recent[] {
    try {
      const list = JSON.parse(localStorage.getItem("isolinear:recent") ?? "[]");
      return Array.isArray(list) ? list.slice(0, 8) : [];
    } catch {
      return [];
    }
  }

  function saveRecents(list: Recent[]) {
    recents = list.slice(0, 8);
    try {
      localStorage.setItem("isolinear:recent", JSON.stringify(recents));
    } catch {
      // Not remembered without storage.
    }
  }

  function flash(text: string) {
    toast = text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), 2200);
  }

  function setTheme(t: Theme) {
    theme = t;
    applyTheme(t);
  }

  /// The ⋯ menu of the toolbar.
  function openMainMenu(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "Open file…", hint: "Ctrl+O", run: chooseFiles },
    ];
    if (doc) {
      const path = doc.path;
      items.push(
        { label: "Open in a new window", run: () => openInNewWindows([path]) },
        { label: "Copy file path", run: () => copyText(path).then(() => flash("Copied the file path")) },
      );
    }
    items.push(
      "separator",
      { label: "System theme", checked: theme === "system", run: () => setTheme("system") },
      { label: "Light", checked: theme === "light", run: () => setTheme("light") },
      { label: "Dark", checked: theme === "dark", run: () => setTheme("dark") },
      "separator",
      { label: "Keyboard shortcuts", hint: "F1", run: () => (shortcutsOpen = true) },
      {
        label: "About Isolinear",
        run: () => {
          aboutOpen = true;
          getVersion().then((v) => (version = v), () => (version = ""));
        },
      },
    );
    menu = { x: r.right - 220, y: r.bottom + 4, items };
  }
  let indexing: IndexProgress | null = $state(null);
  let indent: Indent | null = $state(null);
  let showWhitespace = $state(false);

  // Views this build can show, in the order the format lists them.
  const IMPLEMENTED: View[] = ["overview", "preview", "cells", "tree", "outline", "table", "source", "summary", "strings", "hex"];
  const VIEW_NAMES: Partial<Record<View, string>> = {
    overview: "Overview",
    preview: "Preview",
    cells: "Cells",
    tree: "Tree",
    outline: "Outline",
    table: "Table",
    source: "Source",
    summary: "Summary",
    strings: "Strings",
    hex: "Hex",
  };
  const NO_KEYS: KeyBinding[] = [];
  const ARCHIVES = ["zip", "tar", "gzip", "zstd", "xz", "bzip2"];
  /// A view's name in the switcher: an archive's table is its contents.
  const viewName = (d: DocumentInfo, v: View) =>
    v === "table" && ARCHIVES.includes(d.format.id) ? "Contents" : (VIEW_NAMES[v] ?? v);
  // Views that show the structure panel beside the text.
  const withTree = (v: View) => v === "tree" || v === "outline";

  // Markdown preview, rendered after the text settles.
  let preview = $state<PreviewBlock[] | null>(null);
  let activeBlock = $state(-1);
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  let previewSeq = 0;
  let view: View = $state("source");

  // Table of a delimited file, by revision like the structure. A dialect the user chose is kept
  // across re-indexing after edits.
  let tableRev: number | null = $state(null);
  let tableReady = $state<TableReady | null>(null);
  const tableCurrent = $derived(tableReady !== null && tableReady.rev === tableRev);
  let dialect: { delimiter: string; header: boolean } | null = null;
  let tableView: TableView | undefined = $state();
  let tableTimer: ReturnType<typeof setTimeout> | undefined;
  let tableSeq = 0;
  /// An offset the Hex view opens on, set when an offset is chosen in Strings.
  let hexAt = $state<number | null>(null);
  // First revisions of the open document: anything older belongs to a previous document.
  let tableBase = $state(Infinity);
  let structureBase = $state(Infinity);
  const IMAGES = ["png", "jpeg", "gif", "webp", "bmp", "ico", "tiff"];
  /// Whether this build can show view `v` of `d` (Preview: Markdown and images so far).
  const DOCUMENTS = ["pdf", "docx", "odt"];
  const shows = (d: DocumentInfo, v: View) =>
    IMPLEMENTED.includes(v) &&
    (v !== "preview" || d.format.id === "markdown" || IMAGES.includes(d.format.id) || DOCUMENTS.includes(d.format.id));
  /// Archives this window came through to the open file (an entry opened from a listing).
  let trail = $state<{ path: string; name: string }[]>([]);
  const views = $derived(doc ? doc.views.filter((v) => shows(doc!, v)) : []);
  let imageSize = $state<string | null>(null);

  // Structure: `structureRev` is the revision asked for last; `ready` the last one parsed.
  // The tree is current only when they match.
  let structureRev: number | null = $state(null);
  let ready = $state<StructureReady | null>(null);
  const current = $derived(ready !== null && ready.rev === structureRev ? ready.rev : null);
  let tree: TreeView | undefined = $state();
  // Formatted text (display only) in a second, read-only editor; the source editor keeps its
  // state underneath, edits included.
  let showFormatted = $state(false);
  let formatted = $state<{ id: number; text: string } | null>(null);
  let formatting = $state(false);
  let fmtEditor: Editor | undefined = $state();
  const formattedShown = $derived(showFormatted && mode === "view" && formatted !== null);
  let treeWidth = $state(360);
  let parseTimer: ReturnType<typeof setTimeout> | undefined;
  let editSeq = 0;

  const win = getCurrentWindow();

  async function open(path: string) {
    loading = true;
    error = null;
    try {
      const next = await openDocument(path);
      adopt(next);
    } catch (e) {
      error = `${path}: ${e}`;
      saveRecents(recents.filter((r) => r.path !== path));
    } finally {
      loading = false;
    }
  }

  /// A fact or row of the Overview, shown where it is in the text.
  function showSource(s: OverviewSource) {
    if (!s.text || !doc || doc.text === null) return;
    view = "source";
    const [from, to] = s.text;
    queueMicrotask(() => editor?.select(from, to, true));
  }

  /// Opens the entry on a row of an archive's listing in this window, with a way back.
  async function openEntry(cell: Cell) {
    if (!doc || cell.row === null) return;
    const from = { path: doc.path, name: doc.name };
    try {
      const copy = await archiveExtract(doc.path, doc.format.id, cell.row);
      trail = [...trail, from];
      await open(copy);
    } catch (e) {
      notice = { text: `Cannot open this entry: ${e}`, kind: "error" };
    }
  }

  /// Back to the archive the open entry came from.
  async function back() {
    const previous = trail[trail.length - 1];
    if (!previous) return;
    trail = trail.slice(0, -1);
    await open(previous.path);
  }

  /// Files chosen in the system's open dialog.
  async function chooseFiles() {
    try {
      const chosen = await pickFiles();
      openPaths(chosen);
    } catch (e) {
      notice = { text: `Could not show the open dialog: ${e}`, kind: "error" };
    }
  }

  /// The first path opens here when the window is empty; every other one in a window of its own.
  function openPaths(paths: string[]) {
    if (paths.length === 0) return;
    // Images dropped on a Markdown document being edited become references to them.
    if (doc && doc.format.id === "markdown" && mode === "edit" && paths.every((p) => md.IMAGE_EXTENSIONS.test(p))) {
      editor?.insert(md.imageReferences(paths, doc.path));
      return;
    }
    if (quickPreview && !dirty) {
      trail = [];
      open(paths[0]);
      return;
    }
    const rest = doc || loading ? paths : paths.slice(1);
    if (!doc && !loading) {
      trail = [];
      open(paths[0]);
    }
    if (rest.length > 0)
      openInNewWindows(rest).catch((e) => (notice = { text: `Could not open a window: ${e}`, kind: "error" }));
  }

  function adopt(next: DocumentInfo) {
    doc = next;
    try {
      sessionStorage.setItem("isolinear:path", next.path);
    } catch {
      // Without storage a reload shows the welcome screen.
    }
    if (!next.extracted && !quickPreview) {
      const entry = { path: next.path, name: next.name, format: next.format.id, kind: next.format.kind };
      saveRecents([entry, ...recents.filter((r) => r.path !== next.path)]);
    }
    indexing = null;
    hexAt = null;
    clearTimeout(parseTimer);
    structureRev = next.structure;
    tableRev = next.table;
    preview = null;
    activeBlock = -1;
    clearTimeout(previewTimer);
    if (next.format.id === "markdown" && next.text !== null) renderPreview(next.path, next.text);
    tableBase = next.table ?? Infinity;
    structureBase = next.structure ?? Infinity;
    dialect = null;
    clearTimeout(tableTimer);
    formatted = null;
    showFormatted = next.formattable && next.hardToRead;
    if (showFormatted) refreshFormatted(next.text ?? "");
    const remembered = rememberedView(viewKey(next));
    view =
      remembered && next.views.includes(remembered) && shows(next, remembered)
        ? remembered
        : shows(next, next.suggested)
          ? next.suggested
          : (next.views.find((v) => shows(next, v)) ?? "source");
    imageSize = null;
    baseline = next.text ?? "";
    stamp = next.stamp;
    dirty = false;
    mode = "view";
    disk = null;
    // A binary file shown as decoded text (MessagePack, Protobuf) says so.
    notice = next.format.kind === "binary" && next.text !== null && next.note ? { text: next.note, kind: "info" } : null;
  }

  /// Re-reads the file in place, keeping the mode.
  async function reload() {
    if (!doc) return;
    const keep = mode;
    try {
      const next = await openDocument(doc.path);
      adopt(next);
      mode = next.editable ? keep : "view";
    } catch (e) {
      notice = { text: `Reload failed: ${e}`, kind: "error" };
    }
  }

  function renderPreview(path: string, text: string) {
    const seq = ++previewSeq;
    renderMarkdown(path, text)
      .then((blocks) => {
        if (seq === previewSeq) preview = blocks;
      })
      .catch((e) => (notice = { text: `Preview: ${e}`, kind: "error" }));
  }

  function changed(text: string) {
    dirty = text.length !== baseline.length || text !== baseline;
    if (doc && doc.format.id === "markdown") {
      clearTimeout(previewTimer);
      const path = doc.path;
      previewTimer = setTimeout(() => renderPreview(path, text), 250);
    }
    if (doc && doc.table !== null) {
      const seq = ++tableSeq;
      tableRev = -1;
      clearTimeout(tableTimer);
      const format = doc.format.id;
      tableTimer = setTimeout(() => {
        tableUpdate(format, text, dialect?.delimiter ?? null, dialect?.header ?? null).then((rev) => {
          if (seq === tableSeq) tableRev = rev;
        });
      }, 350);
    }
    if (!doc || doc.structure === null) return;
    // Re-parse once typing pauses; until then the tree shows the previous revision, dimmed.
    // Only the reply to the latest edit names the current revision.
    const seq = ++editSeq;
    structureRev = -1;
    clearTimeout(parseTimer);
    const format = doc.format.id;
    const language = doc.language;
    parseTimer = setTimeout(() => {
      structureUpdate(format, language, text)
        .then((rev) => {
          if (seq === editSeq) structureRev = rev;
        })
        .catch((e) => (notice = { text: `Structure: ${e}`, kind: "error" }));
    }, 350);
  }

  // Diagnostics of the current revision go into the editor's gutter.
  $effect(() => {
    const rev = current;
    const ed = editor;
    if (rev === null || !ed) return;
    treeDiagnostics(rev)
      .then((list) => {
        if (rev === current) ed.showDiagnostics(list);
      })
      .catch(() => {});
  });

  // The same diagnostics, moved to their place in the formatted text.
  $effect(() => {
    const rev = current;
    const ed = fmtEditor;
    const f = formatted;
    if (rev === null || !ed || !f) return;
    treeDiagnostics(rev)
      .then(async (list) => {
        const at = await formattedFromSource(f.id, list.flatMap((d) => [d.from, d.to]));
        ed.showDiagnostics(list.map((d, i) => ({ ...d, from: at[2 * i], to: at[2 * i + 1] })));
      })
      .catch(() => {});
  });

  async function refreshFormatted(text: string) {
    if (!doc) return;
    formatting = true;
    try {
      formatted = await formatText(doc.format.id, text);
    } catch (e) {
      showFormatted = false;
      notice = { text: `Cannot show this file formatted: ${e}`, kind: "warn" };
    } finally {
      formatting = false;
    }
  }

  function toggleFormatted() {
    showFormatted = !showFormatted;
    if (showFormatted) refreshFormatted(editor?.currentText() ?? baseline);
  }

  /// Edit mode: replace the text with its formatted form, as one undoable edit.
  async function reformat() {
    if (!doc || !editor) return;
    try {
      const out = await formatText(doc.format.id, editor.currentText());
      editor.replaceAll(out.text);
    } catch (e) {
      notice = { text: `Cannot reformat: ${e}`, kind: "error" };
    }
  }

  /// The user chose a delimiter or header setting: index the same text again.
  function setDialect(delimiter: string, header: boolean) {
    if (!doc) return;
    dialect = { delimiter, header };
    const seq = ++tableSeq;
    tableRev = -1;
    tableUpdate(doc.format.id, null, delimiter, header).then((rev) => {
      if (seq === tableSeq) tableRev = rev;
    });
  }

  /// A cell opened in the grid: show its text in the source.
  async function openCell(cell: Cell) {
    if (!tableReady || !tableCurrent || !doc || doc.text === null) return;
    try {
      const [from, to] = await tableCellSpan(tableReady.rev, cell.row, cell.column);
      view = "source";
      editor?.select(from, to, true);
    } catch {
      // Re-indexed meanwhile.
    }
  }

  /// The view last chosen for a format, kept per user on this machine.
  /// The choice of view is remembered per standard when the file is in one (a DICOM image and an
  /// unknown binary are both "binary"), else per format.
  const viewKey = (d: DocumentInfo) => (d.standard ? `standard:${d.standard}` : d.format.id);

  function rememberedView(key: string): View | null {
    try {
      return (localStorage.getItem(`view:${key}`) as View | null) ?? null;
    } catch {
      return null;
    }
  }

  /// Switching to the table selects the cell under the editor's cursor.
  function showView(v: View) {
    const from = view;
    view = v;
    try {
      if (doc) localStorage.setItem(`view:${viewKey(doc)}`, v);
    } catch {
      // Storage unavailable: the choice is not remembered.
    }
    if (v === "table" && from !== "table" && tableReady && tableCurrent && editor && doc?.text !== null) {
      tableCellAt(tableReady.rev, editor.cursor())
        .then((cell) => cell && tableView?.reveal(cell))
        .catch(() => {});
    }
  }

  async function pickNode(row: NodeRow) {
    if (formattedShown && formatted) {
      const [from, to] = await formattedFromSource(formatted.id, [row.from, row.to]);
      fmtEditor?.select(from, to);
    } else {
      editor?.select(row.from, row.to);
    }
  }

  /// A tree node's text as written, from the text the tree was parsed from.
  function copyNode(row: NodeRow) {
    const text = editor?.currentText().slice(row.from, row.to);
    if (text === undefined) return;
    copyText(text).then(
      () => {
        flash(`Copied ${row.name ?? row.kind} as written (${text.length.toLocaleString()} characters)`);
      },
      (e) => (notice = { text: `Not copied: ${e}`, kind: "error" }),
    );
  }

  function cursorMoved(offset: number) {
    if (preview) {
      const i = preview.findIndex((b) => offset >= b.from && offset <= b.to);
      activeBlock = i >= 0 ? i : preview.findIndex((b) => b.from > offset) - 1;
    }
    if (!withTree(view) || current === null || !tree) return;
    const rev = current;
    treePath(rev, offset)
      .then((path) => {
        if (rev === current) tree?.reveal(path);
      })
      .catch(() => {});
  }

  function formattedCursorMoved(offset: number) {
    if (!formatted) return;
    formattedToSource(formatted.id, offset)
      .then(cursorMoved)
      .catch(() => {});
  }

  function dragDivider(e: PointerEvent) {
    const start = e.clientX;
    const width = treeWidth;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (m: PointerEvent) => (treeWidth = Math.max(180, Math.min(window.innerWidth - 240, width + m.clientX - start)));
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  }

  function toggleMode() {
    if (!doc?.editable) return;
    mode = mode === "edit" ? "view" : "edit";
    if (mode === "edit") showFormatted = false;
    if (mode === "edit") queueMicrotask(() => editor?.focus());
  }

  async function save(force = false): Promise<boolean> {
    if (!doc || !editor || saving) return false;
    saving = true;
    notice = null;
    const text = editor.currentText();
    try {
      stamp = await saveDocument(doc, text, stamp, force);
      baseline = text;
      dirty = false;
      disk = null;
      doc.size = stamp.size;
      return true;
    } catch (e) {
      const err = e as SaveError;
      if (err.kind === "conflict") {
        disk = "conflict";
      } else {
        notice = { text: `Not saved. ${err.message}`, kind: "error" };
      }
      return false;
    } finally {
      saving = false;
    }
  }

  async function closeWindow(how: "save" | "discard") {
    closeAsk = false;
    if (how === "save" && !(await save())) return;
    dirty = false;
    await win.destroy();
  }

  /// The editor a right-click landed in, if any.
  function editorAt(target: EventTarget | null): EditorView | null {
    const el = target instanceof Element ? target.closest<HTMLElement>(".cm-editor") : null;
    return el ? EditorView.findFromDOM(el) : null;
  }

  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    const target = e.target;
    const view = editorAt(target);
    const field = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement ? target : null;
    let selected = "";
    if (view) {
      selected = view.state.selection.ranges.map((r) => view.state.sliceDoc(r.from, r.to)).join("\n");
    } else if (field) {
      selected = field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0);
    } else {
      selected = window.getSelection()?.toString() ?? "";
    }
    const copy = (text: string, what: string) =>
      copyText(text).then(
        () => flash(`Copied ${what}`),
        (err) => (notice = { text: `Not copied: ${err}`, kind: "error" }),
      );
    const items: MenuItem[] = [];
    if (view && !view.state.readOnly) {
      items.push({
        label: "Cut",
        hint: "Ctrl+X",
        disabled: !selected,
        run: () => copy(selected, "the selection").then(() => view.dispatch(view.state.replaceSelection(""))),
      });
    }
    items.push(
      { label: "Copy", hint: "Ctrl+C", disabled: !selected, run: () => copy(selected, "the selection") },
      {
        label: "Select all",
        hint: "Ctrl+A",
        run: () => {
          if (view) {
            view.focus();
            view.dispatch({ selection: { anchor: 0, head: view.state.doc.length } });
          } else if (field) {
            field.select();
          } else {
            const area = target instanceof Element ? target.closest(".body") : null;
            if (area) window.getSelection()?.selectAllChildren(area);
          }
        },
      },
    );
    if (doc) {
      const path = doc.path;
      items.push("separator", { label: "Copy file path", run: () => copy(path, "the file path") });
    }
    items.push("separator", { label: "Keyboard shortcuts", hint: "F1", run: () => (shortcutsOpen = true) });
    menu = { x: e.clientX, y: e.clientY, items };
  }

  function onKey(e: KeyboardEvent) {
    // The web view's reload would drop the document: F5, Ctrl+R and Ctrl+Shift+R do nothing.
    if (e.key === "F5" || ((e.ctrlKey || e.metaKey) && (e.key === "r" || e.key === "R"))) {
      e.preventDefault();
      return;
    }
    if (e.key === "Escape" && aboutOpen) {
      e.preventDefault();
      aboutOpen = false;
      return;
    }
    if (e.key === "Escape" && closeAsk) {
      e.preventDefault();
      closeAsk = false;
      return;
    }
    // In the quick preview, keys no view uses go to the file manager: arrows select the next
    // file, Space closes the preview (as Escape does).
    const free =
      quickPreview &&
      !e.defaultPrevented &&
      !e.ctrlKey &&
      !e.altKey &&
      !e.metaKey &&
      !shortcutsOpen &&
      !menu &&
      !(e.target instanceof Element && e.target.closest("input, textarea, select, button, .cm-editor, [role=grid], [role=tree], [role=textbox], [role=menu]"));
    const arrow = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" } as const;
    if (free && e.key in arrow) {
      e.preventDefault();
      previewSelect(arrow[e.key as keyof typeof arrow]);
      return;
    }
    if (free && e.key === " " && !dirty) {
      e.preventDefault();
      previewHide();
      return;
    }
    if (quickPreview && e.key === "Escape" && !e.defaultPrevented && !shortcutsOpen && !dirty) {
      e.preventDefault();
      previewHide();
      return;
    }
    if (e.key === "F1") {
      e.preventDefault();
      shortcutsOpen = true;
      return;
    }
    // Alt+1…9 shows the views in switcher order.
    if (e.altKey && !e.ctrlKey && !e.metaKey && /^Digit[1-9]$/.test(e.code)) {
      const v = views[Number(e.code.slice(5)) - 1];
      if (v) {
        e.preventDefault();
        showView(v);
      }
      return;
    }
    if (e.altKey && e.key === "ArrowLeft" && trail.length > 0 && !dirty) {
      e.preventDefault();
      back();
      return;
    }
    const mod = e.ctrlKey || e.metaKey;
    if (!mod || e.altKey) return;
    if (e.key === "o" || e.key === "O") {
      e.preventDefault();
      chooseFiles();
    } else if (e.key === "s" || e.key === "S") {
      e.preventDefault();
      if (dirty) save();
    } else if (e.key === "e" || e.key === "E") {
      e.preventDefault();
      toggleMode();
    }
  }

  onMount(() => {
    previewMode().then((on) => (quickPreview = on));
    // A reload of the page (it should not happen, but a developer build reloads on changes)
    // opens the same file again.
    takePaths().then((paths) => {
      let again: string | null = null;
      try {
        again = sessionStorage.getItem("isolinear:path");
      } catch {
        again = null;
      }
      openPaths(paths.length > 0 || !again ? paths : [again]);
    });
    const unlistenOpen = onOpenPaths(() => takePaths().then(openPaths));
    const unlistenDrop = getCurrentWebview().onDragDropEvent((event) => {
      const type = event.payload.type;
      dragging = type === "enter" || type === "over";
      if (type === "drop") openPaths(event.payload.paths);
    });
    const unlistenDisk = onDiskChange((change) => {
      if (!doc || change.path !== doc.path || sameStamp(change.stamp, stamp)) return;
      if (change.stamp === null) {
        disk = "missing";
        return;
      }
      // Large files are re-indexed only when asked: a growing log would never settle.
      if (!dirty && mode === "view" && doc.paged === null) reload();
      else disk = "changed";
    });
    const unlistenStructure = onStructure((e) => {
      // The reply to a request can arrive after its event; keep the newest.
      if (ready === null || e.rev >= ready.rev) ready = e;
    });
    const unlistenTable = onTable((e) => {
      if (tableReady === null || e.rev >= tableReady.rev) tableReady = e;
    });
    const unlistenClose = win.onCloseRequested((event) => {
      if (dirty) {
        event.preventDefault();
        closeAsk = true;
      } else if (quickPreview) {
        event.preventDefault();
        previewHide();
      }
    });
    return () => {
      unlistenOpen.then((f) => f());
      unlistenDrop.then((f) => f());
      unlistenDisk.then((f) => f());
      unlistenClose.then((f) => f());
      unlistenStructure.then((f) => f());
      unlistenTable.then((f) => f());
    };
  });

  $effect(() => {
    if (doc) win.setTitle(`${dirty ? "• " : ""}${doc.name} — Isolinear`);
  });

  const size = (n: number) =>
    n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1048576).toFixed(1)} MiB`;
  const eol: Record<string, string> = { lf: "LF", "cr-lf": "CRLF", cr: "CR", mixed: "Mixed", none: "" };
  const enc: Record<string, string> = { utf8: "UTF-8", "utf16-le": "UTF-16 LE", "utf16-be": "UTF-16 BE", windows1252: "Windows-1252" };
</script>

<svelte:window onkeydown={onKey} oncontextmenu={onContextMenu} />

<main>
  <header class="toolbar">
    {#if doc}
      {#if trail.length > 0}
        <button class="back" onclick={back} title="Back to {trail[trail.length - 1].name} (Alt+←)" aria-label="Back to {trail[trail.length - 1].name}"
          >←</button
        >
        <span class="crumbs">{trail.map((t) => t.name).join(" › ")} ›</span>
      {/if}
      <FileIcon kind={category(doc.format.id, doc.format.kind, doc.standard)} />
      <span class="name" title={doc.extracted ? "A copy taken out of the archive; read-only" : doc.path}>{doc.name}</span>
      <span class="chip" title={doc.reason}
        >{doc.format.id === "binary" && doc.standard ? doc.standard : doc.format.name}{doc.language && doc.language !== doc.format.id
          ? ` · ${doc.language}`
          : ""}</span
      >
      <span class="grow"></span>
      {#if views.length > 1}
        <div class="segmented" role="group" aria-label="View">
          {#each views as v (v)}
            <button class:on={view === v} onclick={() => showView(v)}>{viewName(doc, v)}</button>
          {/each}
        </div>
      {/if}
      {#if doc.formattable && mode === "view"}
        <button class:on={showFormatted} onclick={toggleFormatted} aria-pressed={showFormatted} title="Show the text indented by its structure (display only)">
          {formatting ? "Formatting…" : "Formatted"}
        </button>
      {/if}
      {#if doc.formattable && mode === "edit" && doc.format.id !== "xml"}
        <button
          onclick={reformat}
          disabled={current === null || (ready?.errors ?? 0) > 0}
          title={(ready?.errors ?? 0) > 0 ? "Fix the errors first" : "Indent the whole text by its structure"}>Reformat</button
        >
      {/if}
      {#if dirty}
        <button class="primary" onclick={() => save()} disabled={saving}>{saving ? "Saving…" : "Save"}</button>
      {/if}
      {#if quickPreview}
        <button
          title="Open this file in a window of its own, to edit it or keep it"
          onclick={() => {
            const path = doc!.path;
            openInNewWindows([path]).then(previewHide, (e) => (notice = { text: `Could not open a window: ${e}`, kind: "error" }));
          }}>Open</button
        >
      {:else if doc.editable}
        <div class="segmented" role="group" aria-label="Mode">
          <button class:on={mode === "view"} onclick={() => (mode = "view")}>View</button>
          <button class:on={mode === "edit"} onclick={() => toggleMode()} disabled={mode === "edit"}>Edit</button>
        </div>
      {:else}
        <span class="readonly" title="This file is shown, not edited">
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"
            ><rect x="2" y="5.5" width="8" height="5.5" rx="1.2" /><path d="M4 5.5V4a2 2 0 0 1 4 0v1.5" /></svg
          >Read-only</span
        >
      {/if}
    {:else}
      <span class="grow"></span>
    {/if}
    <button class="icon-button" onclick={openMainMenu} title="Menu" aria-label="Menu" aria-haspopup="menu">
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"
        ><circle cx="3.5" cy="8" r="1.3" /><circle cx="8" cy="8" r="1.3" /><circle cx="12.5" cy="8" r="1.3" /></svg
      >
    </button>
  </header>

  {#if doc?.format.id === "markdown" && mode === "edit" && view !== "outline"}
    <div class="format-bar" role="toolbar" aria-label="Markdown">
      <button onclick={() => editor?.run(md.wrap("**"))} title="Bold (Ctrl+B)"><b>B</b></button>
      <button onclick={() => editor?.run(md.wrap("*"))} title="Italic (Ctrl+I)"><i>I</i></button>
      <button onclick={() => editor?.run(md.wrap("~~"))} title="Strikethrough"><s>S</s></button>
      <button onclick={() => editor?.run(md.wrap("`"))} title="Inline code (Ctrl+`)"><code>{"<>"}</code></button>
      <span class="sep"></span>
      {#each [1, 2, 3] as n (n)}
        <button onclick={() => editor?.run(md.heading(n))} title="Heading {n} (Ctrl+{n})">H{n}</button>
      {/each}
      <span class="sep"></span>
      <button onclick={() => editor?.run(md.linePrefix("- "))} title="Bulleted list">• List</button>
      <button onclick={() => editor?.run(md.linePrefix("1. "))} title="Numbered list">1. List</button>
      <button onclick={() => editor?.run(md.linePrefix("> "))} title="Quote">❝ Quote</button>
      <span class="sep"></span>
      <button onclick={() => editor?.run((v) => md.link(v))} title="Link (Ctrl+K)">Link</button>
      <button onclick={() => editor?.run((v) => md.link(v, true))} title="Image (Ctrl+Shift+K), or drop image files">Image</button>
      <button
        onclick={() => editor?.run((v) => md.formatTable(v) || (v.dispatch(v.state.replaceSelection("\n| Column | Column |\n| --- | --- |\n|  |  |\n")), true))}
        title="Insert a table, or line up the table under the cursor (Ctrl+Alt+T)">Table</button
      >
    </div>
  {/if}
  {#if disk === "changed"}
    <div class="banner warn">
      <span>The file changed on disk.</span>
      <button onclick={() => reload()}>Reload (discard my edits)</button>
      <button onclick={() => (disk = null)}>{dirty ? "Keep mine" : "Not now"}</button>
    </div>
  {:else if disk === "missing"}
    <div class="banner warn">
      <span>The file was deleted or moved. Save recreates it.</span>
      <button onclick={() => (disk = null)}>OK</button>
    </div>
  {:else if disk === "conflict"}
    <div class="banner warn">
      <span>Not saved: the file changed on disk since you opened it.</span>
      <button onclick={() => save(true)}>Overwrite</button>
      <button onclick={() => reload()}>Reload (discard my edits)</button>
      <button onclick={() => (disk = null)}>Decide later</button>
    </div>
  {/if}
  {#if notice}
    <div class="banner {notice.kind}">
      <span>{notice.text}</span>
      <button onclick={() => (notice = null)}>Dismiss</button>
    </div>
  {/if}

  <section class="body">
    {#if error}
      <div class="state">
        <svg width="28" height="28" viewBox="0 0 24 24" aria-hidden="true"
          ><circle cx="12" cy="12" r="9.5" /><path d="M12 7.5v5.5M12 16.5v.01" /></svg
        >
        <p class="error">{error}</p>
        <button onclick={chooseFiles}>Open another file…</button>
      </div>
    {:else if loading}
      <div class="state"><span class="spinner" aria-hidden="true"></span><p>Opening…</p></div>
    {:else if doc}
      {#if view === "table" && doc.table !== null}
        {#if tableReady && tableReady.rev >= tableBase}
          <TableView
            bind:this={tableView}
            table={tableReady}
            stale={!tableCurrent}
            onopen={doc.text !== null ? openCell : ARCHIVES.includes(doc.format.id) ? openEntry : undefined}
            ondialect={setDialect}
            onswitch={(name) => {
              const d = doc!;
              tableSwitch(d.format.id, d.path, name).then((rev) => (tableRev = rev));
            }}
          />
        {:else}
          <p class="message">Reading the table…</p>
        {/if}
      {/if}
      {#if view === "cells"}
        {#key doc.path}<NotebookView path={doc.path} />{/key}
      {/if}
      {#if view === "overview"}
        <OverviewView path={doc.path} format={doc.format.id} onsource={showSource} />
      {/if}
      {#if doc.text !== null}
      <div class="split" class:hidden={view === "table" || view === "cells" || view === "overview"}>
        {#if withTree(view)}
          <div class="side" style:width="{treeWidth}px">
            <TreeView
              bind:this={tree}
              rev={current ?? (ready && ready.rev >= structureBase ? ready.rev : null)}
              stale={current === null}
              onpick={pickNode}
              oncopy={copyNode}
            />
          </div>
          <div
            class="divider"
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize the structure panel"
            onpointerdown={dragDivider}
          ></div>
        {/if}
        <div class="pane" class:hidden={view === "preview" && mode === "view"}>
          {#if formattedShown && formatted}
            <Editor
              bind:this={fmtEditor}
              text={formatted.text}
              language={doc.language}
              editable={false}
              oncursor={formattedCursorMoved}
            />
          {/if}
          <div class="layer" class:hidden={formattedShown}>
          <Editor
            bind:this={editor}
            text={baseline}
            language={doc.language}
            editable={mode === "edit"}
            onchange={changed}
            oncursor={cursorMoved}
            onindent={(i) => (indent = i)}
            keys={doc.format.id === "markdown" ? md.markdownKeymap : NO_KEYS}
            {showWhitespace}
          />
          </div>
        </div>
        {#if view === "preview"}
          {#if mode === "edit"}<div class="seam"></div>{/if}
          <div class="pane">
            {#if preview}
              <MarkdownPreview
                blocks={preview}
                active={mode === "edit" ? activeBlock : -1}
                onpick={(b) => mode === "edit" && editor?.select(b.from, b.from, true)}
              />
            {:else}
              <p class="message">Rendering…</p>
            {/if}
          </div>
        {/if}
      </div>
      {:else if doc.paged !== null}
        <div class="fill" class:hidden={view === "table" || view === "cells" || view === "overview"}>
          {#key doc.paged}
            <PagedView id={doc.paged} onprogress={(p) => (indexing = p)} />
          {/key}
        </div>
      {:else if view === "preview" && doc.format.id === "pdf"}
        {#key doc.path}<PdfView path={doc.path} />{/key}
      {:else if view === "preview" && DOCUMENTS.includes(doc.format.id)}
        <DocumentView path={doc.path} format={doc.format.id} />
      {:else if view === "preview" && IMAGES.includes(doc.format.id)}
        <ImageView path={doc.path} format={doc.format.id} onsize={(w, h) => (imageSize = `${w} × ${h}`)} />
      {:else if view === "summary"}
        <SummaryView {doc} />
      {:else if view === "strings"}
        {#key doc.path}<StringsView path={doc.path} size={doc.size} onoffset={(o) => ((hexAt = o), (view = "hex"))} />{/key}
      {:else if view === "hex"}
        {#key doc.path}<HexView path={doc.path} size={doc.size} at={hexAt} />{/key}
      {:else if view !== "table" && view !== "cells" && view !== "overview"}
        <p class="message">{doc.format.name}: {size(doc.size)}. {doc.note ?? "No view for this file yet."}</p>
      {/if}
    {:else}
      <Welcome
        {recents}
        onchoose={chooseFiles}
        onopen={(path) => open(path)}
        onforget={(path) => saveRecents(recents.filter((r) => r.path !== path))}
      />
    {/if}
  </section>

  <footer class="status" class:hidden={!doc}>
    {#if doc}
      <span>{size(doc.size)}</span>
      {#if doc.encoding}<span>{enc[doc.encoding]}{doc.bom ? " BOM" : ""}</span>{/if}
      {#if doc.lineEnding && eol[doc.lineEnding]}<span>{eol[doc.lineEnding]}</span>{/if}
      {#if mode === "edit"}<span class="chip">Editing</span>{/if}
      {#if dirty}<span class="chip unsaved">Unsaved</span>{/if}
      {#if doc.lossy}<span class="warn" title="Shown with replacement characters; read-only, so the file keeps its bytes">Some bytes could not be decoded</span>{/if}
      {#if imageSize}<span>{imageSize} pixels</span>{/if}
      {#if doc.table !== null && tableReady && tableReady.rev >= tableBase}
        <span>{tableReady.rows.toLocaleString()} rows × {tableReady.columns} columns</span>
        {#if tableReady.unterminated !== null}<span class="error">A quote is never closed</span>{/if}
        {#if tableReady.ragged}<span class="warn" title="Some rows have more or fewer fields than the first">Uneven rows</span>{/if}
      {/if}
      {#if doc.text !== null && indent}
        <button
          class="link"
          class:on={showWhitespace}
          onclick={() => (showWhitespace = !showWhitespace)}
          aria-pressed={showWhitespace}
          title="Indentation found in the file. Click to show spaces and tabs."
          >{indent.tabs ? "Tabs" : `Spaces: ${indent.width}`}</button
        >
        {#if indent.inconsistent > 0}
          <span class="warn" title="Lines indented differently from the rest of the file; marked while editing"
            >{indent.inconsistent} {indent.inconsistent === 1 ? "line" : "lines"} indented differently</span
          >
        {/if}
      {/if}
      {#if doc.structure !== null}
        {#if current === null}
          <span>Reading structure…</span>
        {:else if ready}
          {#if ready.errors > 0}<span class="error">{ready.errors} {ready.errors === 1 ? "error" : "errors"}</span>{/if}
          {#if ready.warnings > 0}<span class="warn">{ready.warnings} {ready.warnings === 1 ? "warning" : "warnings"}</span>{/if}
          {#if ready.stopped === "depth"}<span class="warn">Structure stops at the nesting limit</span>{/if}
          {#if ready.stopped === "nodes"}<span class="warn">Structure stops at 4 million nodes</span>{/if}
        {/if}
      {/if}
      {#if doc.paged !== null}
        <span>Large file, read-only</span>
        {#if indexing && !indexing.complete}
          <span aria-live="polite">Reading lines… {Math.floor((100 * indexing.scanned) / Math.max(1, indexing.size))}%</span>
        {:else if indexing}
          <span>{indexing.lines.toLocaleString()} lines</span>
        {/if}
      {/if}
      <span class="grow"></span>
      <span class="muted">{doc.reason}</span>
    {/if}
  </footer>

  {#if dragging}
    <div class="drop" aria-hidden="true"><div>Drop to open</div></div>
  {/if}
  {#if toast}
    <div class="toast" role="status">{toast}</div>
  {/if}
  {#if aboutOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="scrim" onclick={() => (aboutOpen = false)}>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="dialog about" role="dialog" aria-label="About Isolinear" tabindex="-1" onclick={(e) => e.stopPropagation()}>
        <h2>Isolinear {version}</h2>
        <p>A local-first viewer and editor for structured text, data and industry files. Nothing in a file is run, nothing is fetched.</p>
        <p>Built by mlaib with Claude (Anthropic) as co-author. Apache-2.0.</p>
        <div class="actions"><button class="primary" onclick={() => (aboutOpen = false)}>Close</button></div>
      </div>
    </div>
  {/if}
  {#if menu}
    <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
  {/if}
  {#if shortcutsOpen}
    <Shortcuts onclose={() => (shortcutsOpen = false)} />
  {/if}
  {#if closeAsk}
    <div class="scrim" role="presentation">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="close-title">
        <h2 id="close-title">Save changes to {doc?.name}?</h2>
        <p>Your changes will be lost if you don't save them.</p>
        <div class="actions">
          <button onclick={() => closeWindow("discard")}>Don't save</button>
          <button onclick={() => (closeAsk = false)}>Cancel</button>
          <button class="primary" onclick={() => closeWindow("save")}>Save</button>
        </div>
      </div>
    </div>
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  .toolbar,
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--bar);
    font-size: 12.5px;
  }
  .toolbar {
    height: 42px;
    flex: none;
    padding: 0 8px 0 12px;
    border-bottom: 1px solid var(--line);
  }
  .status {
    height: 26px;
    flex: none;
    padding: 0 12px;
    gap: 14px;
    border-top: 1px solid var(--line);
    color: var(--fg-muted);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
  }
  .name {
    font-weight: 600;
    font-size: 13.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--chip);
    color: var(--fg-muted);
    font-size: 11.5px;
    white-space: nowrap;
  }
  .chip.unsaved {
    background: var(--warn-bg);
    color: var(--warn);
  }
  .status.hidden {
    display: none;
  }
  .readonly {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--fg-muted);
    font-size: 12px;
    padding: 0 4px;
    white-space: nowrap;
  }
  .readonly svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.2;
  }
  .icon-button {
    display: grid;
    place-items: center;
    width: 30px;
    height: 28px;
    padding: 0;
    border-color: transparent;
    background: transparent;
    box-shadow: none;
    color: var(--fg-muted);
  }
  .icon-button:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .icon-button svg {
    fill: currentColor;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .split,
  .fill {
    display: flex;
    height: 100%;
  }
  .fill > :global(*) {
    flex: 1;
  }
  .split.hidden,
  .fill.hidden {
    display: none;
  }
  .side {
    flex: none;
    min-width: 0;
    border-right: 1px solid var(--line);
  }
  .divider {
    flex: none;
    width: 5px;
    margin-left: -3px;
    cursor: col-resize;
    position: relative;
    z-index: 1;
  }
  .divider:hover {
    background: var(--accent);
    opacity: 0.4;
  }
  .pane {
    flex: 1;
    min-width: 0;
    position: relative;
  }
  .pane.hidden {
    display: none;
  }
  .seam {
    flex: none;
    width: 1px;
    background: var(--line);
  }
  .layer {
    height: 100%;
  }
  .layer.hidden {
    display: none;
  }
  .state {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 24px;
    color: var(--fg-muted);
    text-align: center;
  }
  .state p {
    margin: 0;
    max-width: 560px;
    overflow-wrap: anywhere;
  }
  .state svg {
    fill: none;
    stroke: var(--danger);
    stroke-width: 1.6;
    stroke-linecap: round;
  }
  .spinner {
    width: 22px;
    height: 22px;
    border: 2px solid var(--line);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .message {
    padding: 24px;
    color: var(--fg-muted);
  }
  .error {
    color: var(--danger);
  }
  .warn {
    color: var(--warn);
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--fg-muted);
    opacity: 0.8;
  }
  button {
    font: inherit;
    font-size: 12.5px;
    height: 28px;
    padding: 0 11px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--raised);
    color: var(--fg);
    cursor: pointer;
    box-shadow: var(--shadow-sm);
    white-space: nowrap;
  }
  button:hover:not(:disabled) {
    background: var(--hover);
  }
  button:disabled {
    cursor: default;
    opacity: 0.55;
  }
  button.on {
    background: var(--accent-soft);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--line));
    color: var(--accent);
  }
  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  button.primary:hover:not(:disabled) {
    background: var(--accent);
    filter: brightness(1.08);
  }
  .segmented {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border-radius: 8px;
    background: var(--chip);
  }
  .segmented button {
    height: 24px;
    padding: 0 10px;
    border: none;
    border-radius: 6px;
    background: transparent;
    box-shadow: none;
    color: var(--fg-muted);
  }
  .segmented button:hover:not(:disabled) {
    background: transparent;
    color: var(--fg);
  }
  .segmented button.on {
    background: var(--active);
    color: var(--fg);
    font-weight: 600;
    box-shadow: var(--shadow-sm);
    opacity: 1;
  }
  button.link {
    height: auto;
    border: none;
    background: none;
    box-shadow: none;
    padding: 0;
    color: inherit;
    font-size: inherit;
  }
  button.link:hover:not(:disabled) {
    background: none;
  }
  button.link:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  .back {
    padding: 1px 8px;
  }
  .crumbs {
    color: var(--fg-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 30vw;
  }
  .format-bar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--line);
    background: var(--bar);
  }
  .format-bar button {
    height: 26px;
    border-color: transparent;
    background: transparent;
    box-shadow: none;
    padding: 2px 8px;
  }
  .format-bar button:hover {
    background: var(--chip);
  }
  .format-bar .sep {
    width: 1px;
    height: 16px;
    background: var(--line);
    margin: 0 4px;
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 12px;
    font-size: 12.5px;
    border-bottom: 1px solid var(--line);
  }
  .banner.warn {
    background: var(--warn-bg);
    color: var(--warn);
  }
  .banner.error {
    background: var(--danger-bg);
    color: var(--danger);
  }
  .banner.info {
    background: var(--bar);
  }
  .toast {
    position: fixed;
    left: 50%;
    bottom: 40px;
    transform: translateX(-50%);
    z-index: 90;
    padding: 7px 14px;
    border-radius: 8px;
    background: var(--fg);
    color: var(--bg);
    font-size: 12.5px;
    box-shadow: var(--shadow-lg);
    animation: rise 0.15s ease-out;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 6px);
    }
  }
  .drop {
    position: fixed;
    inset: 8px;
    z-index: 95;
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: 12px;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    pointer-events: none;
  }
  .drop div {
    padding: 10px 20px;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .about p {
    line-height: 1.5;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.35);
    display: grid;
    place-items: center;
  }
  .dialog {
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 18px 20px;
    min-width: 360px;
    max-width: 480px;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.25);
  }
  .dialog h2 {
    margin: 0 0 8px;
    font-size: 15px;
  }
  .dialog p {
    margin: 0 0 16px;
    color: var(--fg-muted);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
