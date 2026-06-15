// Theme preference: system / light / dark. Applied as a `.dark` class on `<html>`
// (Tailwind `@custom-variant dark` in index.css). Accent themes (e.g. `.theme-ocean`)
// are a second class layered on top — see THEMES registry + index.css stub.

export type Theme = "system" | "light" | "dark";

export type ThemeDefinition = {
  id: Theme;
  label: string;
  /** Swatch fill for the settings picker card. */
  previewBg: string;
};

export const THEMES: ThemeDefinition[] = [
  {
    id: "system",
    label: "System",
    previewBg:
      "linear-gradient(135deg, oklch(0.95 0.01 200) 50%, oklch(0.25 0.02 195) 50%)",
  },
  { id: "light", label: "Light", previewBg: "oklch(0.987 0.004 200)" },
  { id: "dark", label: "Dark", previewBg: "oklch(0.215 0.018 195)" },
];

/** Resolve the effective light/dark mode for a stored preference.
 *  When changing system-mode logic, also update the inline boot script in
 *  index.html (runs pre-module and duplicates this check). */
export function resolveEffectiveTheme(theme: Theme): "light" | "dark" {
  if (theme === "light") return "light";
  if (theme === "dark") return "dark";
  if (typeof window === "undefined") return "dark";
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

let systemListenerAttached = false;

/** Toggle `.dark` on `<html>` from the stored preference (not the effective mode). */
export function applyTheme(theme: Theme): void {
  const effective = resolveEffectiveTheme(theme);
  document.documentElement.classList.toggle("dark", effective === "dark");
}

/** Re-apply when OS appearance changes while preference is `"system"`. */
export function setupSystemThemeListener(getPreference: () => Theme): void {
  if (systemListenerAttached || typeof window === "undefined") return;
  systemListenerAttached = true;
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  mq.addEventListener("change", () => {
    if (getPreference() === "system") applyTheme("system");
  });
}

// React hook: re-exported from the unified prefs store.
export { useTheme } from "@/store/prefs";
