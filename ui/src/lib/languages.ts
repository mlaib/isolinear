// Editor modes for language names the editor does not know by that name. `null` means plain text:
// no mode comes close enough.
export const EDITOR_MODE: Record<string, string | null> = {
  makefile: "shell",
  elixir: "ruby",
  svelte: "html",
  wast: "webassembly",
  nix: null,
};

/// The editor mode to load for a detected language.
export function editorMode(language: string | null): string | null {
  if (language === null) return null;
  return language in EDITOR_MODE ? EDITOR_MODE[language] : language;
}
