// Light/dark theme preference. Dark is the default (low-chrome Zed/Linear
// aesthetic, per PRINCIPLES); the choice persists in localStorage and is applied
// as a `.dark` class on <html> (Tailwind custom variant defined in index.css).

import { create } from "zustand";

export type Theme = "light" | "dark";

const STORAGE_KEY = "ff-theme";

function loadTheme(): Theme {
  // Dark by default; an explicit "light" choice still wins.
  return localStorage.getItem(STORAGE_KEY) === "light" ? "light" : "dark";
}

function applyTheme(theme: Theme): void {
  document.documentElement.classList.toggle("dark", theme === "dark");
}

interface ThemeState {
  theme: Theme;
  toggleTheme: () => void;
}

export const useTheme = create<ThemeState>((set) => ({
  theme: loadTheme(),
  toggleTheme: () =>
    set((s) => {
      const next: Theme = s.theme === "light" ? "dark" : "light";
      localStorage.setItem(STORAGE_KEY, next);
      applyTheme(next);
      return { theme: next };
    }),
}));

// Apply the persisted choice immediately on module load, before first paint.
applyTheme(loadTheme());
