import { describe, expect, it } from "vitest";
import { formatRows } from "./copyOut";

const header = ["name", "note"];
const rows = [
  ["alpha", 'says "hi", twice'],
  ["beta", null],
  ["gamma", "a|b\nc\td"],
];

describe("formatRows", () => {
  it("writes CSV with quoting and CRLF", () => {
    expect(formatRows(header, rows, "csv")).toBe(
      'name,note\r\nalpha,"says ""hi"", twice"\r\nbeta,\r\ngamma,"a|b\nc\td"\r\n',
    );
    expect(formatRows(null, [[" padded", "x"]], "csv")).toBe('" padded",x\r\n');
  });

  it("writes TSV with tabs and line breaks folded", () => {
    expect(formatRows(header, rows, "tsv")).toBe('name\tnote\nalpha\tsays "hi", twice\nbeta\t\ngamma\ta|b c d\n');
  });

  it("writes a Markdown table with escaped pipes and named columns", () => {
    expect(formatRows(header, rows, "markdown")).toBe(
      '| name | note |\n| --- | --- |\n| alpha | says "hi", twice |\n| beta |  |\n| gamma | a\\|b<br>c\td |\n',
    );
    expect(formatRows(null, [["1", "2"]], "markdown")).toBe("| Column 1 | Column 2 |\n| --- | --- |\n| 1 | 2 |\n");
  });

  it("writes JSON objects with unique keys, nulls kept", () => {
    const out = JSON.parse(formatRows(["a", "a", "b"], [["1", "2", null]], "json"));
    expect(out).toEqual([{ a: "1", a_2: "2", b: null }]);
    expect(JSON.parse(formatRows(null, [["1"], ["2", "3"]], "json"))).toEqual([
      ["1", null],
      ["2", "3"],
    ]);
  });
});
