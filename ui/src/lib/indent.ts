// Indentation style of a text, guessed from its first lines the way most editors do.

export interface Indent {
  tabs: boolean;
  /// Spaces per level; the tab width when indenting with tabs.
  width: number;
  /// Lines indented the other way, or with tabs and spaces mixed (in the sample).
  inconsistent: number;
}

export function detectIndent(text: string, fallback = { tabs: false, width: 4 }): Indent {
  const sample = text.length > 65536 ? text.slice(0, 65536) : text;
  let tabLines = 0;
  let spaceLines = 0;
  let mixedLines = 0;
  const steps = new Map<number, number>();
  let previous = 0;
  for (const line of sample.split("\n")) {
    if (line.trim() === "") continue;
    if (/^[ \t]*(\t +\S| +\t)/.test(line)) mixedLines++;
    if (line.startsWith("\t")) {
      tabLines++;
      previous = 0;
      continue;
    }
    const spaces = line.length - line.trimStart().length;
    if (spaces > 0) spaceLines++;
    const step = Math.abs(spaces - previous);
    if (step >= 2 && step <= 8) steps.set(step, (steps.get(step) ?? 0) + 1);
    previous = spaces;
  }
  if (tabLines === 0 && spaceLines === 0) return { ...fallback, inconsistent: 0 };
  if (tabLines > spaceLines) return { tabs: true, width: fallback.width, inconsistent: spaceLines + mixedLines };
  let best = fallback.width;
  let count = 0;
  for (const [step, n] of steps) {
    if (n > count || (n === count && step < best)) {
      best = step;
      count = n;
    }
  }
  return { tabs: false, width: best, inconsistent: tabLines + mixedLines };
}
