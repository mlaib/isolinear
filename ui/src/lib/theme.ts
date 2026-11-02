// Editor look, from the CSS variables in app.css so light and dark follow the system.
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";

const chrome = EditorView.theme({
  "&": { color: "var(--fg)", backgroundColor: "var(--bg)" },
  ".cm-content": { caretColor: "var(--fg)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--fg)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": {
    backgroundColor: "var(--selection)",
  },
  ".cm-gutters": {
    backgroundColor: "var(--bar)",
    color: "var(--fg-muted)",
    borderRight: "1px solid var(--line)",
  },
  ".cm-activeLine": { backgroundColor: "var(--active-line)" },
  ".cm-activeLineGutter": { backgroundColor: "var(--active-line)", color: "var(--fg)" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--chip)", border: "none", color: "var(--fg-muted)" },
  ".cm-searchMatch": { backgroundColor: "var(--warn-bg)", outline: "1px solid var(--warn)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--selection)" },
  ".cm-selectionMatch": { backgroundColor: "var(--chip)" },
  "&.cm-focused .cm-matchingBracket": { backgroundColor: "var(--chip)", outline: "1px solid var(--line)" },
  ".cm-panels": { backgroundColor: "var(--bar)", color: "var(--fg)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--line)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--line)" },
  ".cm-textfield": {
    backgroundColor: "var(--bg)",
    color: "var(--fg)",
    border: "1px solid var(--line)",
    borderRadius: "5px",
  },
  ".cm-button": {
    backgroundImage: "none",
    backgroundColor: "var(--bg)",
    color: "var(--fg)",
    border: "1px solid var(--line)",
    borderRadius: "5px",
  },
  ".cm-tooltip": { backgroundColor: "var(--bar)", color: "var(--fg)", border: "1px solid var(--line)" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--danger)" },
  ".cm-diagnostic-warning": { borderLeftColor: "var(--warn)" },
  ".cm-structure-node": { backgroundColor: "var(--node)" },
  ".cm-trailingSpace": { backgroundColor: "var(--danger-bg)" },
  ".cm-inconsistent-indent": { backgroundColor: "var(--warn-bg)" },
  ".cm-highlightSpace": { backgroundImage: "radial-gradient(circle at 50% 55%, var(--fg-muted) 11%, transparent 5%)", opacity: "0.5" },
  ".cm-highlightTab": { opacity: "0.5" },
});

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.operatorKeyword, t.modifier, t.controlKeyword], color: "var(--syn-keyword)" },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--syn-string)" },
  { tag: [t.number, t.bool, t.null, t.atom, t.unit], color: "var(--syn-number)" },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: [t.propertyName, t.definition(t.propertyName)], color: "var(--syn-property)" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.labelName], color: "var(--syn-function)" },
  { tag: [t.typeName, t.className, t.namespace, t.definition(t.typeName)], color: "var(--syn-type)" },
  { tag: [t.tagName, t.angleBracket], color: "var(--syn-tag)" },
  { tag: [t.attributeName], color: "var(--syn-attribute)" },
  { tag: [t.meta, t.processingInstruction, t.documentMeta], color: "var(--syn-comment)" },
  { tag: t.heading, color: "var(--syn-heading)", fontWeight: "600" },
  { tag: t.strong, fontWeight: "600" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: [t.link, t.url], color: "var(--syn-link)", textDecoration: "underline" },
  { tag: [t.quote, t.monospace], color: "var(--syn-string)" },
  { tag: [t.escape, t.character], color: "var(--syn-number)" },
  { tag: t.invalid, color: "var(--danger)" },
]);

export const editorTheme = [chrome, syntaxHighlighting(highlight)];
