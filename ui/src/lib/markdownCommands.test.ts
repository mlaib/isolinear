import { describe, expect, it } from "vitest";
import { EditorSelection, EditorState, type TransactionSpec } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { formatTable, heading, imageReferences, link, linePrefix, wrap } from "./markdownCommands";
import { detectIndent } from "./indent";

/// The part of an editor the commands use: its state, and dispatch.
function editor(doc: string, from: number, to = from) {
  let state = EditorState.create({
    doc,
    selection: EditorSelection.single(from, to),
    extensions: EditorState.allowMultipleSelections.of(true),
  });
  const view = {
    get state() {
      return state;
    },
    dispatch(...specs: TransactionSpec[]) {
      state = state.update(...specs).state;
    },
  } as unknown as EditorView;
  return {
    view,
    text: () => state.doc.toString(),
    selected: () => state.sliceDoc(state.selection.main.from, state.selection.main.to),
  };
}

describe("wrap", () => {
  it("wraps the selection and unwraps it again", () => {
    const e = editor("a word here", 2, 6);
    wrap("**")(e.view);
    expect(e.text()).toBe("a **word** here");
    expect(e.selected()).toBe("word");
    wrap("**")(e.view);
    expect(e.text()).toBe("a word here");
  });

  it("puts the cursor between the marks when nothing is selected", () => {
    const e = editor("x", 1);
    wrap("`")(e.view);
    expect(e.text()).toBe("x``");
    expect(e.view.state.selection.main.head).toBe(2);
  });
});

describe("link", () => {
  it("selects the URL to type over", () => {
    const e = editor("see docs", 4, 8);
    link(e.view);
    expect(e.text()).toBe("see [docs](url)");
    expect(e.selected()).toBe("url");
    const i = editor("", 0);
    link(i.view, true);
    expect(i.text()).toBe("![alt text](url)");
  });
});

describe("heading", () => {
  it("sets, changes and removes the level", () => {
    const e = editor("Title", 0);
    heading(2)(e.view);
    expect(e.text()).toBe("## Title");
    heading(1)(e.view);
    expect(e.text()).toBe("# Title");
    heading(0)(e.view);
    expect(e.text()).toBe("Title");
  });
});

describe("linePrefix", () => {
  it("toggles a list over the selected lines and numbers them", () => {
    const e = editor("a\nb\nc", 0, 5);
    linePrefix("1. ")(e.view);
    expect(e.text()).toBe("1. a\n2. b\n3. c");
    linePrefix("- ")(e.view);
    expect(e.text()).toBe("- a\n- b\n- c");
    linePrefix("- ")(e.view);
    expect(e.text()).toBe("a\nb\nc");
  });

  it("keeps indentation", () => {
    const e = editor("  item", 0);
    linePrefix("> ")(e.view);
    expect(e.text()).toBe("  > item");
  });
});

describe("formatTable", () => {
  it("lines up the columns and keeps alignment", () => {
    const e = editor("text\n|a|bb|\n|:-|-:|\n|ccc|d|\nafter", 7);
    expect(formatTable(e.view)).toBe(true);
    expect(e.text()).toBe("text\n| a   | bb  |\n| :-- | --: |\n| ccc | d   |\nafter");
  });

  it("keeps escaped pipes in their cell", () => {
    const e = editor("| a \\| b | c |\n|---|---|\n| 1 | 2 |", 0);
    formatTable(e.view);
    expect(e.text().split("\n")[0]).toBe("| a \\| b | c   |");
  });

  it("does nothing outside a table", () => {
    const e = editor("not a table", 0);
    expect(formatTable(e.view)).toBe(false);
    expect(e.text()).toBe("not a table");
  });
});

describe("imageReferences", () => {
  it("makes paths relative to the document and brackets awkward ones", () => {
    expect(imageReferences(["/docs/img/a.png", "/docs/my pic.jpg", "/other/b.gif"], "/docs/readme.md")).toBe(
      "![a](img/a.png)\n![my pic](<my pic.jpg>)\n![b](/other/b.gif)",
    );
    expect(imageReferences(["C:\\docs\\x.png"], "C:\\docs\\readme.md")).toBe("![x](x.png)");
  });
});

describe("detectIndent", () => {
  it("finds tabs, two and four spaces", () => {
    expect(detectIndent("a\n\tb\n\t\tc\n")).toEqual({ tabs: true, width: 4, inconsistent: 0 });
    expect(detectIndent("{\n  \"a\": {\n    \"b\": 1\n  }\n}")).toEqual({ tabs: false, width: 2, inconsistent: 0 });
    expect(detectIndent("def f():\n    return 1\n")).toEqual({ tabs: false, width: 4, inconsistent: 0 });
    expect(detectIndent("flat\ntext\n")).toEqual({ tabs: false, width: 4, inconsistent: 0 });
  });

  it("counts lines that disagree with the file", () => {
    expect(detectIndent("a\n  b\n    c\n\td\n").inconsistent).toBe(1);
    expect(detectIndent("a\n\tb\n\t\tc\n  d\n").inconsistent).toBe(1);
    expect(detectIndent("a\n  \tb\n").inconsistent).toBe(1);
  });
});
