// Unified display preferences — theme + font. Persisted to localStorage under
// `"ff-prefs"` via zustand/middleware; `subscribe` applies side-effects on change.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { applyFont, type Font } from "@/lib/fonts";
import {
  applyTheme,
  resolveEffectiveTheme,
  setupSystemThemeListener,
  type Theme,
} from "@/lib/theme";

const STORAGE_KEY = "ff-prefs";
const LEGACY_THEME_KEY = "ff-theme";

export interface PrefsState {
  theme: Theme;
  font: Font;
  setTheme: (theme: Theme) => void;
  setFont: (font: Font) => void;
  /** Quick light/dark flip — leaves `"system"` by picking the opposite effective mode. */
  toggleTheme: () => void;
}

/** Lift a pre-#62 `ff-theme` value into prefs on first rehydrate. Exported for tests. */
export function migrateLegacyTheme(): Partial<Pick<PrefsState, "theme">> {
  try {
    const legacy = localStorage.getItem(LEGACY_THEME_KEY);
    if (legacy === "light" || legacy === "dark") {
      localStorage.removeItem(LEGACY_THEME_KEY);
      return { theme: legacy };
    }
  } catch {
    // localStorage unavailable (SSR/tests) — ignore.
  }
  return {};
}

export const usePrefsStore = create<PrefsState>()(
  persist(
    (set, get) => ({
      theme: "system",
      font: "geist",
      setTheme: (theme) => set({ theme }),
      setFont: (font) => set({ font }),
      toggleTheme: () => {
        const { theme } = get();
        const effective = resolveEffectiveTheme(theme);
        set({ theme: effective === "light" ? "dark" : "light" });
      },
    }),
    {
      name: STORAGE_KEY,
      merge: (persisted, current) => ({
        ...current,
        ...(persisted as Partial<PrefsState>),
        ...migrateLegacyTheme(),
      }),
      onRehydrateStorage: () => (state) => {
        if (state) {
          applyTheme(state.theme);
          applyFont(state.font);
        }
      },
    },
  ),
);

let initialized = false;

/** Wire apply side-effects and the OS theme listener. Call once on app mount. */
export function initPrefs(): void {
  if (initialized) return;
  initialized = true;

  const { theme, font } = usePrefsStore.getState();
  applyTheme(theme);
  applyFont(font);
  setupSystemThemeListener(() => usePrefsStore.getState().theme);

  usePrefsStore.subscribe((state) => {
    applyTheme(state.theme);
    applyFont(state.font);
  });
}

/** @alias usePrefsStore — backward-compatible hook name for theme consumers. */
export { usePrefsStore as useTheme };
