// Rows of a table as text for the clipboard: CSV, TSV, a Markdown table or JSON. Copying is an
// explicit action; nothing here writes a file.

export type CopyFormat = "csv" | "tsv" | "markdown" | "json";

export const COPY_FORMATS: { id: CopyFormat; name: string }[] = [
  { id: "csv", name: "CSV" },
  { id: "tsv", name: "TSV" },
  { id: "markdown", name: "Markdown table" },
  { id: "json", name: "JSON" },
];

type Cells = (string | null)[];

/// Column names: the header where it has one, else "Column n".
function names(header: string[] | null, columns: number): string[] {
  return Array.from({ length: columns }, (_, c) => header?.[c] ?? `Column ${c + 1}`);
}

function csvCell(v: string | null): string {
  const s = v ?? "";
  return /[",\r\n]/.test(s) || /^\s|\s$/.test(s) ? `"${s.replaceAll('"', '""')}"` : s;
}

/// Tabs and line breaks would split a TSV cell, so they become spaces.
const tsvCell = (v: string | null) => (v ?? "").replace(/[\t\r\n]+/g, " ");

const markdownCell = (v: string | null) =>
  (v ?? "").replace(/\\/g, "\\\\").replace(/\|/g, "\\|").replace(/\r?\n|\r/g, "<br>");

/// JSON keys from the column names, made unique ("name", "name_2").
function keys(columns: string[]): string[] {
  const seen = new Map<string, number>();
  return columns.map((name) => {
    const n = (seen.get(name) ?? 0) + 1;
    seen.set(name, n);
    return n === 1 ? name : `${name}_${n}`;
  });
}

/// `rows` as text; `header` is written as the first line (CSV, TSV, Markdown) or as the keys of
/// each object (JSON). Without a header, CSV and TSV have no header line, Markdown and JSON use
/// "Column n".
export function formatRows(header: string[] | null, rows: Cells[], format: CopyFormat): string {
  const columns = Math.max(header?.length ?? 0, ...rows.map((r) => r.length), 0);
  const pad = (r: Cells) => Array.from({ length: columns }, (_, c) => r[c] ?? null);
  switch (format) {
    case "csv": {
      const lines = rows.map((r) => pad(r).map(csvCell).join(","));
      if (header) lines.unshift(pad(header).map(csvCell).join(","));
      return lines.map((l) => l + "\r\n").join("");
    }
    case "tsv": {
      const lines = rows.map((r) => pad(r).map(tsvCell).join("\t"));
      if (header) lines.unshift(pad(header).map(tsvCell).join("\t"));
      return lines.map((l) => l + "\n").join("");
    }
    case "markdown": {
      const head = names(header, columns).map(markdownCell);
      const line = (cells: string[]) => `| ${cells.join(" | ")} |\n`;
      return line(head) + line(head.map(() => "---")) + rows.map((r) => line(pad(r).map(markdownCell))).join("");
    }
    case "json": {
      if (!header) return JSON.stringify(rows.map(pad), null, 2) + "\n";
      const k = keys(names(header, columns));
      const objects = rows.map((r) => Object.fromEntries(pad(r).map((v, c) => [k[c], v])));
      return JSON.stringify(objects, null, 2) + "\n";
    }
  }
}
