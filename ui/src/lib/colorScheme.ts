// The colour theme: the system's, or light or dark chosen in the app (remembered per user).

export type Theme = "system" | "light" | "dark";

export function savedTheme(): Theme {
  try {
    const t = localStorage.getItem("isolinear:theme");
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(theme: Theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem("isolinear:theme", theme);
  } catch {
    // Not remembered without storage.
  }
}
