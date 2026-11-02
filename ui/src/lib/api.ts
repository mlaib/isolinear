// Typed calls into the Rust side. Shapes mirror `src-tauri/src/main.rs`.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

export type View =
  | "source"
  | "formatted"
  | "tree"
  | "table"
  | "preview"
  | "outline"
  | "cells"
  | "hex"
  | "summary"
  | "strings"
  | "overview";

export type Tier = "editable" | "view-only" | "paged";
export type Encoding = "utf8" | "utf16-le" | "utf16-be" | "windows1252";
export type LineEnding = "lf" | "cr-lf" | "cr" | "mixed" | "none";

export interface Stamp {
  size: number;
  modifiedSecs: number;
  modifiedNanos: number;
}

export interface DocumentInfo {
  path: string;
  name: string;
  size: number;
  tier: Tier;
  stamp: Stamp;
  format: { id: string; name: string; kind: "text" | "binary" };
  language: string | null;
  reason: string;
  views: View[];
  suggested: View;
  editable: boolean;
  text: string | null;
  encoding: Encoding | null;
  bom: boolean;
  lineEnding: LineEnding | null;
  lossy: boolean;
  /// Index id when the text is read a page at a time.
  paged: number | null;
  /// Why there is no view, when there is none.
  note: string | null;
  /// Revision of the structure tree being parsed, when the format has one.
  structure: number | null;
  /// The industry standard the file is in, when one is recognized.
  standard: string | null;
  /// A copy taken out of an archive: read-only.
  extracted: boolean;
  /// Revision of the table being indexed, for delimited formats.
  table: number | null;
  /// The text can be shown formatted; and should be by default when hard to read.
  formattable: boolean;
  hardToRead: boolean;
}

export type NodeKind =
  | "document"
  | "object"
  | "array"
  | "table"
  | "part"
  | "record"
  | "element"
  | "attribute"
  | "text"
  | "cData"
  | "comment"
  | "processingInstruction"
  | "doctype"
  | "alias"
  | "scalar"
  | "section"
  | "symbol";

export type ScalarKind = "string" | "number" | "boolean" | "null" | "date" | "other";

export interface NodeRow {
  id: number;
  kind: NodeKind;
  scalar: ScalarKind | null;
  name: string | null;
  value: string | null;
  children: number;
  line: number | null;
  /// UTF-16 offsets of the node in the text it was parsed from.
  from: number;
  to: number;
}

export interface PathStep {
  id: number;
  index: number;
}

export interface StructureReady {
  rev: number;
  nodes: number;
  errors: number;
  warnings: number;
  stopped: "error" | "depth" | "nodes" | "cancelled" | null;
}

export interface DiagnosticRow {
  severity: "error" | "warning" | "info";
  message: string;
  from: number;
  to: number;
}

export interface IndexProgress {
  lines: number;
  scanned: number;
  size: number;
  complete: boolean;
}

export interface IndexEvent extends IndexProgress {
  id: number;
  error: string | null;
}

export interface PagedLine {
  text: string;
  bytes: number;
  truncated: boolean;
}

export interface Found {
  line: number;
  /// UTF-16 units into the shown line text; null beyond the shown part.
  column: number | null;
  length: number;
  byteOffset: number;
}

/// Bytes of a line shown at most (matches `paged::LINE_CAP`).
export const LINE_CAP = 32 * 1024;

export type SaveError =
  | { kind: "conflict"; stamp: Stamp | null }
  | { kind: "unencodable"; message: string }
  | { kind: "io"; message: string };

export interface DiskChange {
  path: string;
  stamp: Stamp | null;
}

/// Paths waiting to be opened (command line, or sent by the system); each returned once.
export function takePaths(): Promise<string[]> {
  return invoke("take_paths");
}

export function onOpenPaths(handler: () => void): Promise<UnlistenFn> {
  return listen("open-paths", () => handler());
}

export function openInNewWindows(paths: string[]): Promise<void> {
  return invoke("open_in_new_windows", { paths });
}

export function pagedLines(
  id: number,
  start: number,
  count: number,
): Promise<IndexProgress & { page: PagedLine[] }> {
  return invoke("paged_lines", { id, start, count });
}

export function pagedFind(
  id: number,
  query: string,
  from: { line: number } | { offset: number },
  matchCase: boolean,
): Promise<{ found: Found | null; cancelled: boolean }> {
  return invoke("paged_find", {
    id,
    query,
    fromLine: "line" in from ? from.line : null,
    fromOffset: "offset" in from ? from.offset : null,
    matchCase,
  });
}

export function onIndexProgress(handler: (e: IndexEvent) => void): Promise<UnlistenFn> {
  return listen<IndexEvent>("index-progress", (e) => handler(e.payload));
}

export function openDocument(path: string): Promise<DocumentInfo> {
  return invoke("open_document", { path });
}

export function saveDocument(
  doc: DocumentInfo,
  text: string,
  expected: Stamp | null,
  force: boolean,
): Promise<Stamp> {
  return invoke("save_document", {
    path: doc.path,
    text,
    encoding: doc.encoding,
    bom: doc.bom,
    lineEnding: doc.lineEnding,
    expected,
    force,
  });
}

export function onDiskChange(handler: (change: DiskChange) => void): Promise<UnlistenFn> {
  return listen<DiskChange>("disk-changed", (e) => handler(e.payload));
}

export function sameStamp(a: Stamp | null, b: Stamp | null): boolean {
  if (a === null || b === null) return a === b;
  return a.size === b.size && a.modifiedSecs === b.modifiedSecs && a.modifiedNanos === b.modifiedNanos;
}

/// Parses the editor's text after an edit; resolves to the new revision.
export function structureUpdate(format: string, language: string | null, text: string): Promise<number> {
  return invoke("structure_update", { format, language, text });
}

export function onStructure(handler: (e: StructureReady) => void): Promise<UnlistenFn> {
  return listen<StructureReady>("structure", (e) => handler(e.payload));
}

export function treeChildren(
  rev: number,
  node: number,
  start: number,
  count: number,
): Promise<{ rows: NodeRow[]; total: number }> {
  return invoke("tree_children", { rev, node, start, count });
}

export function treePath(rev: number, offset: number): Promise<PathStep[]> {
  return invoke("tree_path", { rev, offset });
}

export function treeDiagnostics(rev: number): Promise<DiagnosticRow[]> {
  return invoke("tree_diagnostics", { rev });
}

/// Formats text for display (or for the Reformat command); the id names its offset map.
export function formatText(format: string, text: string): Promise<{ id: number; text: string }> {
  return invoke("format_text", { format, text });
}

export function formattedFromSource(id: number, offsets: number[]): Promise<number[]> {
  return invoke("formatted_from_source", { id, offsets });
}

export function formattedToSource(id: number, offset: number): Promise<number> {
  return invoke("formatted_to_source", { id, offset });
}

export interface TableReady {
  rev: number;
  rows: number;
  columns: number;
  header: string[] | null;
  /// Delimiter and header choice; null for tables that are not delimited text (BibTeX).
  dialect: { delimiter: string; hasHeader: boolean } | null;
  ragged: boolean;
  unterminated: number | null;
  error: string | null;
  /// Tables of a multi-table file (sheets, datasets) and the one open.
  tables: { name: string; rows: number | null }[];
  table: number;
  /// Column types, when the format has them.
  types: string[] | null;
  details: [string, string][];
  schemaText: string | null;
  /// Rows can be sorted and filtered.
  arrangeable: boolean;
}

export interface Cell {
  /// Data row in file order; null on the header.
  row: number | null;
  column: number;
  /// Where the row is in the current order and filter; null when filtered out.
  position?: number | null;
}

export interface TableRow {
  /// Row number in file order.
  n: number;
  /// null is a null value.
  cells: (string | null)[];
}

export function onTable(handler: (e: TableReady) => void): Promise<UnlistenFn> {
  return listen<TableReady>("table", (e) => handler(e.payload));
}

/// Indexes again: new text after an edit, or (text null) the same text with another dialect.
export function tableUpdate(
  format: string,
  text: string | null,
  delimiter: string | null,
  header: boolean | null,
): Promise<number> {
  return invoke("table_update", { format, text, delimiter, header });
}

export function tableRows(rev: number, start: number, count: number): Promise<{ rows: TableRow[] }> {
  return invoke("table_rows", { rev, start, count });
}

/// Sorts by a column and filters by text; resolves to the number of rows shown, and for a large
/// binary file how many of its first rows were sorted and filtered.
export function tableArrange(
  rev: number,
  column: number | null,
  descending: boolean,
  filter: string,
): Promise<{ shown: number; within: number | null }> {
  return invoke("table_arrange", { rev, column, descending, filter });
}

export function tableCellSpan(rev: number, row: number | null, column: number): Promise<[number, number]> {
  return invoke("table_cell_span", { rev, row, column });
}

export function tableCellAt(rev: number, offset: number): Promise<Cell | null> {
  return invoke("table_cell_at", { rev, offset });
}

export interface PreviewBlock {
  html: string;
  /// UTF-16 range of the block in the text.
  from: number;
  to: number;
}

/// Markdown rendered block by block; images next to `path` are embedded, remote ones are not.
export function renderMarkdown(path: string, text: string): Promise<PreviewBlock[]> {
  return invoke("render_markdown", { path, text });
}

/// Opens another table (sheet, dataset) of a multi-table file.
export function tableSwitch(format: string, path: string, table: string): Promise<number> {
  return invoke("table_switch", { format, path, table });
}

/// Bytes of rows `start..start + count` (16 bytes per row).
export function hexRows(path: string, start: number, count: number): Promise<{ offset: number; bytes: number[]; size: number }> {
  return invoke("hex_rows", { path, start, count });
}

export function fileSummary(path: string): Promise<{ facts: [string, string][] }> {
  return invoke("file_summary", { path });
}

/// Readable runs in the first bytes of a file: offset and text, in file order.
export function fileStrings(path: string): Promise<{ strings: [number, string][]; scanned: number; cut: boolean }> {
  return invoke("file_strings", { path });
}

/// An image file as a data URI.
export function imageData(path: string, format: string): Promise<string> {
  return invoke("image_data", { path, format });
}

export type NotebookOutput =
  | { kind: "stream"; name: string; text: string }
  | { kind: "text"; text: string }
  | { kind: "image"; uri: string; alt: string }
  | { kind: "html"; html: string }
  | { kind: "source"; mime: string; text: string }
  | { kind: "error"; name: string; value: string; traceback: string }
  | { kind: "unsupported"; what: string };

export type NotebookCell =
  | { kind: "code"; source: string; executionCount: number | null; outputs: NotebookOutput[] }
  | { kind: "markdown"; html: string }
  | { kind: "raw"; source: string };

export interface Notebook {
  language: string | null;
  kernel: string | null;
  nbformat: string;
  cells: NotebookCell[];
}

/// A notebook's cells and saved outputs; nothing is executed.
export function readNotebook(path: string): Promise<Notebook> {
  return invoke("notebook", { path });
}

/// Files chosen in the system's open dialog (none when cancelled).
export async function pickFiles(): Promise<string[]> {
  const chosen = await openDialog({ multiple: true, directory: false, title: "Open" });
  if (chosen === null) return [];
  return Array.isArray(chosen) ? chosen : [chosen];
}

export function pdfInfo(path: string): Promise<{ pages: number }> {
  return invoke("pdf_info", { path });
}

/// Page `index` (from 0) as a PNG data URI, `scale` times its size in points.
export function pdfPage(path: string, index: number, scale: number): Promise<string> {
  return invoke("pdf_page", { path, index, scale });
}

/// A Word or OpenDocument text file as HTML built by the reader.
export function documentHtml(path: string, format: string): Promise<string> {
  return invoke("document_html", { path, format });
}

/// Copies an archive entry (a row of its listing) to this window's private folder; its path.
export function archiveExtract(path: string, format: string, row: number): Promise<string> {
  return invoke("archive_extract", { path, format, row });
}

export interface OverviewSource {
  /// Byte range in the file.
  from: number;
  to: number;
  /// UTF-16 range in the decoded text, for text files.
  text: [number, number] | null;
}

export type OverviewVisual =
  | { kind: "map"; features: { name: string; shape: "points" | "lines" | "polygons"; parts: [number, number][][] }[]; geographic: boolean }
  | { kind: "plot"; channels: { name: string; unit: string; points: [number, number][]; samples: number }[] }
  | { kind: "drawing"; paths: [number, number][][]; unit: string }
  | { kind: "picture"; uri: string; caption: string }
  | { kind: "image"; width: number; height: number; rgba: string; caption: string }
  | { kind: "timeline"; rows: { label: string; start: number; parts: number[]; source: OverviewSource | null }[]; phases: string[]; unit: string }
  | { kind: "mesh"; vertices: number[]; triangles: number[]; shown: number; total: number }
  | { kind: "note"; text: string };

export interface Overview {
  family: string;
  kind: string;
  version: string | null;
  checks: { stage: string; state: "passed" | "partial" | "failed" | "notDone"; detail: string }[];
  facts: { group: string; label: string; value: string; source: OverviewSource | null }[];
  tables: { name: string; columns: string[]; rows: { cells: string[]; source: OverviewSource | null }[]; total: number }[];
  outline: { depth: number; label: string; value: string; source: OverviewSource | null }[];
  outlineTotal: number;
  visuals: OverviewVisual[];
  notes: string[];
}

/// What the reader of the file's industry standard found; null when none recognizes it.
export function inspectDocument(path: string, format: string): Promise<Overview | null> {
  return invoke("inspect_document", { path, format });
}

/// Puts text on the clipboard. The window cannot read the clipboard.
export function copyText(text: string): Promise<void> {
  return writeText(text);
}

/// Whether this window is the file manager's quick preview (GNOME Files, Space on a file).
export function previewMode(): Promise<boolean> {
  return invoke("preview_mode");
}

/// Hides the quick preview; the process stays for the next file.
export function previewHide(): Promise<void> {
  return invoke("preview_hide");
}

/// Asks the file manager to move its selection (an arrow key in the quick preview).
export function previewSelect(direction: "up" | "down" | "left" | "right"): Promise<void> {
  return invoke("preview_select", { direction });
}
