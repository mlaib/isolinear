// Markdown editing aids: each command edits the text as one undoable change.
import { EditorSelection, type ChangeSpec } from "@codemirror/state";
import type { EditorView, KeyBinding } from "@codemirror/view";

/// Wraps each selection in `mark` (or removes it when already wrapped); an empty selection
/// gets the marks with the cursor between them.
export function wrap(mark: string) {
  return (view: EditorView): boolean => {
    const { state } = view;
    view.dispatch(
      state.changeByRange((range) => {
        const before = state.sliceDoc(range.from - mark.length, range.from);
        const after = state.sliceDoc(range.to, range.to + mark.length);
        if (before === mark && after === mark) {
          return {
            changes: [
              { from: range.from - mark.length, to: range.from },
              { from: range.to, to: range.to + mark.length },
            ],
            range: EditorSelection.range(range.from - mark.length, range.to - mark.length),
          };
        }
        return {
          changes: [
            { from: range.from, insert: mark },
            { from: range.to, insert: mark },
          ],
          range: EditorSelection.range(range.from + mark.length, range.to + mark.length),
        };
      }),
    );
    return true;
  };
}

/// `[selection](url)` with "url" selected, ready to type over.
export function link(view: EditorView, image = false): boolean {
  const { state } = view;
  const r = state.selection.main;
  const text = state.sliceDoc(r.from, r.to) || (image ? "alt text" : "text");
  const prefix = image ? "![" : "[";
  const insert = `${prefix}${text}](url)`;
  const urlFrom = r.from + prefix.length + text.length + 2;
  view.dispatch({
    changes: { from: r.from, to: r.to, insert },
    selection: EditorSelection.range(urlFrom, urlFrom + 3),
  });
  return true;
}

/// Lines touched by the selections, each once.
function selectedLines(view: EditorView) {
  const seen = new Set<number>();
  const lines = [];
  for (const r of view.state.selection.ranges) {
    for (let n = view.state.doc.lineAt(r.from).number; n <= view.state.doc.lineAt(r.to).number; n++) {
      if (!seen.has(n)) {
        seen.add(n);
        lines.push(view.state.doc.line(n));
      }
    }
  }
  return lines;
}

/// Sets the heading level of the selected lines (0 removes it).
export function heading(level: number) {
  return (view: EditorView): boolean => {
    const changes: ChangeSpec[] = selectedLines(view).map((line) => {
      const m = /^(#{1,6})\s+/.exec(line.text);
      const insert = level > 0 ? "#".repeat(level) + " " : "";
      return { from: line.from, to: line.from + (m ? m[0].length : 0), insert };
    });
    view.dispatch({ changes });
    return true;
  };
}

const PREFIXES: Record<string, RegExp> = {
  "- ": /^(\s*)[-*+] (\[[ xX]\] )?/,
  "1. ": /^(\s*)\d+[.)] /,
  "> ": /^(\s*)> ?/,
};

/// Toggles a list or quote prefix on the selected lines; numbered lists are numbered in order.
export function linePrefix(prefix: "- " | "1. " | "> ") {
  return (view: EditorView): boolean => {
    const lines = selectedLines(view).filter((l) => l.text.trim() !== "" || view.state.selection.main.empty);
    const re = PREFIXES[prefix];
    const all = lines.every((l) => re.test(l.text));
    const changes: ChangeSpec[] = lines.map((line, i) => {
      const m = re.exec(line.text);
      if (all && m) return { from: line.from + m[1].length, to: line.from + m[0].length };
      // Replace another kind of list marker rather than stacking them.
      const other = Object.values(PREFIXES)
        .map((r) => r.exec(line.text))
        .find((x) => x && x !== null);
      const indent = /^\s*/.exec(line.text)![0].length;
      const insert = prefix === "1. " ? `${i + 1}. ` : prefix;
      if (other) return { from: line.from + other[1].length, to: line.from + other[0].length, insert };
      return { from: line.from + indent, insert };
    });
    view.dispatch({ changes });
    return true;
  };
}

/// Pads the cells of the table under the cursor so its columns line up. Alignment colons in the
/// delimiter row are kept. Returns false (doing nothing) when the cursor is not in a table.
export function formatTable(view: EditorView): boolean {
  const { doc } = view.state;
  const here = doc.lineAt(view.state.selection.main.head).number;
  const isRow = (n: number) => n >= 1 && n <= doc.lines && /^\s*\|.*\|\s*$/.test(doc.line(n).text);
  if (!isRow(here)) return false;
  let first = here;
  let last = here;
  while (isRow(first - 1)) first--;
  while (isRow(last + 1)) last++;
  const rows: string[][] = [];
  for (let n = first; n <= last; n++) {
    rows.push(splitRow(doc.line(n).text));
  }
  const delimiter = rows.findIndex((r) => r.every((c) => /^:?-+:?$/.test(c.trim())));
  if (delimiter !== 1) return false;
  const columns = Math.max(...rows.map((r) => r.length));
  const width = (s: string) => [...s].length;
  const widths = Array.from({ length: columns }, (_, c) =>
    Math.max(3, ...rows.filter((_, i) => i !== delimiter).map((r) => width((r[c] ?? "").trim()))),
  );
  const text = rows
    .map((r, i) =>
      "| " +
      widths
        .map((w, c) => {
          const cell = (r[c] ?? "").trim();
          if (i !== delimiter) return cell + " ".repeat(w - width(cell));
          const left = cell.startsWith(":");
          const right = cell.endsWith(":") && cell.length > 1;
          return (left ? ":" : "-") + "-".repeat(w - 2) + (right ? ":" : "-");
        })
        .join(" | ") +
      " |",
    )
    .join("\n");
  view.dispatch({ changes: { from: doc.line(first).from, to: doc.line(last).to, insert: text } });
  return true;
}

/// Cells of a table row; a backslash-escaped pipe stays inside its cell.
function splitRow(line: string): string[] {
  const body = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  const cells: string[] = [];
  let cell = "";
  for (let i = 0; i < body.length; i++) {
    if (body[i] === "\\" && body[i + 1] === "|") {
      cell += "\\|";
      i++;
    } else if (body[i] === "|") {
      cells.push(cell);
      cell = "";
    } else {
      cell += body[i];
    }
  }
  cells.push(cell);
  return cells;
}

/// Image references for dropped files: relative to the document when below its folder.
export function imageReferences(paths: string[], documentPath: string): string {
  const sep = documentPath.includes("\\") && !documentPath.includes("/") ? "\\" : "/";
  const dir = documentPath.slice(0, documentPath.lastIndexOf(sep) + 1);
  return paths
    .map((p) => {
      const rel = p.startsWith(dir) ? p.slice(dir.length) : p;
      const target = rel.split(sep).join("/");
      const name = target.slice(target.lastIndexOf("/") + 1).replace(/\.[^.]+$/, "");
      return `![${name}](${/[\s()<>]/.test(target) ? `<${target}>` : target})`;
    })
    .join("\n");
}

export const IMAGE_EXTENSIONS = /\.(png|jpe?g|gif|webp|svg|bmp|ico|avif)$/i;

export const markdownKeymap: KeyBinding[] = [
  { key: "Mod-b", run: wrap("**") },
  { key: "Mod-i", run: wrap("*") },
  { key: "Mod-k", run: (v) => link(v) },
  { key: "Mod-Shift-k", run: (v) => link(v, true) },
  { key: "Mod-`", run: wrap("`") },
  { key: "Mod-0", run: heading(0) },
  ...[1, 2, 3, 4, 5, 6].map((n) => ({ key: `Mod-${n}`, run: heading(n) })),
  { key: "Mod-Alt-t", run: formatTable },
];
