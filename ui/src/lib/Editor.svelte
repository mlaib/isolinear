<script lang="ts">
  // CodeMirror 6 host. Read-only unless `editable`; the language is loaded on demand from
  // the editor's language list by the name the Rust side detected. The document is held
  // LF-separated; a file with mixed line endings keeps its CR characters visible.
  import { onMount } from "svelte";
  import { Annotation, EditorState, Compartment, StateEffect, StateField, EditorSelection } from "@codemirror/state";
  import {
    EditorView,
    Decoration,
    lineNumbers,
    highlightActiveLine,
    highlightActiveLineGutter,
    highlightSpecialChars,
    drawSelection,
    dropCursor,
    rectangularSelection,
    crosshairCursor,
    highlightTrailingWhitespace,
    highlightWhitespace,
    MatchDecorator,
    ViewPlugin,
    type DecorationSet,
    type ViewUpdate,
    keymap,
  } from "@codemirror/view";
  import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
  import { bracketMatching, foldGutter, foldKeymap, indentOnInput, indentUnit, LanguageDescription } from "@codemirror/language";
  import { searchKeymap, highlightSelectionMatches, gotoLine } from "@codemirror/search";
  import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
  import { lintGutter, setDiagnostics, type Diagnostic } from "@codemirror/lint";
  import { languages } from "@codemirror/language-data";
  import type { KeyBinding } from "@codemirror/view";
  import { editorTheme } from "./theme";
  import { detectIndent, type Indent } from "./indent";
  import { editorMode } from "./languages";

  let {
    text,
    language,
    editable = false,
    onchange,
    oncursor,
    onindent,
    keys = [],
    showWhitespace = false,
  }: {
    text: string;
    language: string | null;
    editable?: boolean;
    onchange?: (text: string) => void;
    /// The main cursor moved (UTF-16 offset), after typing stops or on a click.
    oncursor?: (offset: number) => void;
    onindent?: (indent: Indent) => void;
    /// Extra key bindings, ahead of the defaults (Markdown shortcuts).
    keys?: KeyBinding[];
    /// Draw spaces and tabs.
    showWhitespace?: boolean;
  } = $props();

  let host: HTMLDivElement;
  let view: EditorView | undefined;
  const languageConf = new Compartment();
  const editableConf = new Compartment();
  const indentConf = new Compartment();
  const keysConf = new Compartment();
  const comfortConf = new Compartment();
  const whitespaceConf = new Compartment();
  let currentIndent: Indent = { tabs: false, width: 4, inconsistent: 0 };

  /// Marks indentation that disagrees with the file's style, or mixes tabs and spaces.
  function inconsistentIndent(indent: Indent) {
    const marker = new MatchDecorator({
      regexp: indent.tabs ? /^(?: +\t[ \t]*|\t* {2,})/g : /^ *\t[ \t]*/g,
      decoration: Decoration.mark({ class: "cm-inconsistent-indent", attributes: { title: "Indented differently from the rest of the file" } }),
    });
    return ViewPlugin.fromClass(
      class {
        decorations: DecorationSet;
        constructor(view: EditorView) {
          this.decorations = marker.createDeco(view);
        }
        update(u: ViewUpdate) {
          this.decorations = marker.updateDeco(u, this.decorations);
        }
      },
      { decorations: (v) => v.decorations },
    );
  }

  function comfort(on: boolean, indent: Indent) {
    return on ? [highlightTrailingWhitespace(), inconsistentIndent(indent)] : [];
  }

  // Text set by the program (a new document, a reload), not typed: no change is reported.
  const external = Annotation.define<boolean>();

  // The structure node selected in the tree, drawn as a band behind the text.
  const setNode = StateEffect.define<{ from: number; to: number } | null>();
  const nodeField = StateField.define({
    create: () => Decoration.none,
    update(deco, tr) {
      deco = deco.map(tr.changes);
      for (const e of tr.effects) {
        if (e.is(setNode)) {
          deco =
            e.value && e.value.to > e.value.from
              ? Decoration.set([Decoration.mark({ class: "cm-structure-node" }).range(e.value.from, e.value.to)])
              : Decoration.none;
        }
      }
      return deco;
    },
    provide: (f) => EditorView.decorations.from(f),
  });

  export function focus() {
    view?.focus();
  }

  /// The main cursor's UTF-16 offset.
  export function cursor(): number {
    return view ? view.state.selection.main.head : 0;
  }

  export function currentText(): string {
    return view ? view.state.doc.toString() : text;
  }

  /// Selects `from..to` (UTF-16 offsets), marks it, and scrolls it into view.
  export function select(from: number, to: number, focusEditor = false) {
    if (!view) return;
    const len = view.state.doc.length;
    from = Math.min(from, len);
    to = Math.min(to, len);
    view.dispatch({
      selection: EditorSelection.single(from, from),
      effects: [setNode.of({ from, to }), EditorView.scrollIntoView(from, { y: "center" })],
    });
    if (focusEditor) view.focus();
  }

  export function showDiagnostics(list: Diagnostic[]) {
    if (!view) return;
    const len = view.state.doc.length;
    const clamped = list.map((d) => ({ ...d, from: Math.min(d.from, len), to: Math.min(Math.max(d.to, d.from), len) }));
    view.dispatch(setDiagnostics(view.state, clamped));
  }

  /// Runs an editing command on the text, then gives the editor the focus.
  export function run(command: (view: EditorView) => boolean) {
    if (!view) return;
    command(view);
    view.focus();
  }

  /// Inserts text at the cursor (replacing the selection), as one undoable edit.
  export function insert(text: string) {
    if (!view) return;
    view.dispatch(view.state.replaceSelection(text));
    view.focus();
  }

  /// Replaces the whole text as one undoable edit.
  export function replaceAll(next: string) {
    if (!view) return;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: next } });
  }

  async function loadLanguage(language: string | null) {
    const name = editorMode(language);
    if (!name) return [];
    const desc = LanguageDescription.matchLanguageName(languages, name, true);
    if (!desc) return [];
    const support = await desc.load();
    return [support];
  }

  function editableExtensions(on: boolean) {
    return [EditorView.editable.of(on), EditorState.readOnly.of(!on)];
  }

  function indentExtensions(indent: Indent) {
    return [indentUnit.of(indent.tabs ? "\t" : " ".repeat(indent.width)), EditorState.tabSize.of(indent.width)];
  }

  let cursorTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    const indent = detectIndent(text);
    currentIndent = indent;
    onindent?.(indent);
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: text,
        extensions: [
          EditorState.lineSeparator.of("\n"),
          EditorState.allowMultipleSelections.of(true),
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          history(),
          foldGutter(),
          lintGutter(),
          drawSelection(),
          dropCursor(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          rectangularSelection(),
          crosshairCursor(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          editorTheme,
          nodeField,
          keysConf.of(keymap.of(keys)),
          keymap.of([
            { key: "Mod-g", run: gotoLine, preventDefault: true },
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...historyKeymap,
            ...searchKeymap,
            ...foldKeymap,
            indentWithTab,
          ]),
          editableConf.of(editableExtensions(editable)),
          indentConf.of(indentExtensions(indent)),
          comfortConf.of(comfort(editable, indent)),
          whitespaceConf.of(showWhitespace ? highlightWhitespace() : []),
          languageConf.of([]),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !u.transactions.some((t) => t.annotation(external))) onchange?.(u.state.doc.toString());
            if (u.selectionSet && oncursor) {
              const head = u.state.selection.main.head;
              clearTimeout(cursorTimer);
              cursorTimer = setTimeout(() => oncursor?.(head), u.docChanged ? 400 : 60);
            }
          }),
        ],
      }),
    });
    return () => {
      clearTimeout(cursorTimer);
      view?.destroy();
    };
  });

  // A new document replaces the whole state; the language follows asynchronously.
  $effect(() => {
    const current = view;
    const t = text;
    const lang = language;
    if (!current) return;
    if (current.state.doc.toString() !== t) {
      const indent = detectIndent(t);
      currentIndent = indent;
      onindent?.(indent);
      current.dispatch({
        changes: { from: 0, to: current.state.doc.length, insert: t },
        effects: [
          indentConf.reconfigure(indentExtensions(indent)),
          comfortConf.reconfigure(comfort(editable, indent)),
          setNode.of(null),
        ],
        annotations: external.of(true),
      });
    }
    loadLanguage(lang).then((ext) => {
      if (view === current) current.dispatch({ effects: languageConf.reconfigure(ext) });
    });
  });

  $effect(() => {
    view?.dispatch({
      effects: [
        editableConf.reconfigure(editableExtensions(editable)),
        comfortConf.reconfigure(comfort(editable, currentIndent)),
      ],
    });
  });

  $effect(() => {
    const on = showWhitespace;
    view?.dispatch({ effects: whitespaceConf.reconfigure(on ? highlightWhitespace() : []) });
  });

  $effect(() => {
    const k = keys;
    view?.dispatch({ effects: keysConf.reconfigure(keymap.of(k)) });
  });
</script>

<div class="editor" bind:this={host}></div>

<style>
  .editor {
    height: 100%;
    overflow: hidden;
  }
  .editor :global(.cm-editor) {
    height: 100%;
    font-size: 13px;
  }
  .editor :global(.cm-scroller) {
    font-family: var(--mono);
    line-height: 1.5;
  }
  .editor :global(.cm-editor.cm-focused) {
    outline: none;
  }
</style>
