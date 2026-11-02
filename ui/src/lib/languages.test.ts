import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { editorMode } from "./languages";

// Every language name the Rust side can report must name an editor language, or files of that
// language open without highlighting. The names are read from the Rust sources themselves.
const source = (path: string) => readFileSync(fileURLToPath(new URL(path, import.meta.url)), "utf8");

function rustLanguageNames(): string[] {
  const names = new Set<string>();
  const lang = source("../../../crates/formats/src/language.rs");
  for (const m of lang.matchAll(/\("[^"]*",\s*"([^"]+)"\)/g)) names.add(m[1]);
  for (const m of lang.matchAll(/=> "([a-z+#-]+)",/g)) names.add(m[1]);
  const registry = source("../../../crates/formats/src/registry.rs");
  for (const m of registry.matchAll(/language: Some\("([^"]+)"\)/g)) names.add(m[1]);
  return [...names].sort();
}

describe("language names", () => {
  it("are read from the Rust sources", () => {
    expect(rustLanguageNames().length).toBeGreaterThan(40);
  });

  it("all resolve to an editor language, or are plain text on purpose", () => {
    const missing = rustLanguageNames().filter((n) => {
      const mode = editorMode(n);
      return mode !== null && !LanguageDescription.matchLanguageName(languages, mode, true);
    });
    expect(missing).toEqual([]);
  });
});
