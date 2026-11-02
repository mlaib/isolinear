// Finishing touches on rendered content: math through KaTeX, code highlighted by the editor's
// language parsers. Shared by the Markdown preview and notebook cells.
import katex from "katex";
import "katex/dist/katex.min.css";
import { highlightCode, classHighlighter } from "@lezer/highlight";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { editorMode } from "./languages";

/// Renders `.math` elements (TeX in `data-tex`) with KaTeX; macros cannot run code or load files.
export function renderMath(root: HTMLElement) {
  for (const el of root.querySelectorAll<HTMLElement>(".math")) {
    try {
      katex.render(el.dataset.tex ?? "", el, {
        displayMode: el.classList.contains("display"),
        throwOnError: false,
        trust: false,
        maxExpand: 1000,
        maxSize: 50,
      });
    } catch {
      el.textContent = el.dataset.tex ?? "";
    }
  }
}

/// Highlights `code` as `language`, replacing its text with styled spans.
export function highlightInto(code: HTMLElement, language: string | null) {
  const mode = editorMode(language);
  const desc = mode ? LanguageDescription.matchLanguageName(languages, mode, true) : null;
  if (!desc) return;
  const text = code.textContent ?? "";
  desc.load().then((support) => {
    if (!code.isConnected) return;
    const out = document.createDocumentFragment();
    highlightCode(
      text,
      support.language.parser.parse(text),
      classHighlighter,
      (t, cls) => {
        if (!cls) return out.append(document.createTextNode(t));
        const span = document.createElement("span");
        span.className = cls;
        span.textContent = t;
        out.append(span);
      },
      () => out.append("\n"),
    );
    code.replaceChildren(out);
  });
}

/// Math and fenced code blocks (`language-…` classes) inside rendered Markdown.
export function enhance(root: HTMLElement) {
  renderMath(root);
  for (const code of root.querySelectorAll<HTMLElement>("pre > code[class*='language-']")) {
    const lang = [...code.classList].find((c) => c.startsWith("language-"))?.slice(9) ?? null;
    highlightInto(code, lang);
  }
}
