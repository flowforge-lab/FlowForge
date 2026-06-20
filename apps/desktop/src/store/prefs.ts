// Unified display preferences — theme, font, font scale, display name,
// notifications, open-thread budget. Persisted to localStorage under `"ff-prefs"`
// via zustand/middleware; `subscribe` applies side-effects on change.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import {
  applyFont,
  applyFontScale,
  clampFontScale,
  type Font,
} from "@/lib/fonts";
import {
  applyTheme,
  resolveEffectiveTheme,
  setupSystemThemeListener,
  type Theme,
} from "@/lib/theme";
import type { Mode } from "@/bindings";

const STORAGE_KEY = "ff-prefs";
const LEGACY_THEME_KEY = "ff-theme";

/** FE-only notification flags (SET.2). No OS notifications fired yet. */
export interface NotificationPrefs {
  enabled: boolean;
  messageComplete: boolean;
  approvalRequests: boolean;
  sound: boolean;
}

/** Bounds for the open-thread budget (max sessions kept loaded, LRU). */
export const OPEN_THREADS_MIN = 3;
export const OPEN_THREADS_MAX = 20;

/** Bounds + default for the drag-resizable session sidebar width, px (#204).
 *  Default matches the previous fixed `w-60` (15rem). */
export const SIDEBAR_WIDTH_MIN = 200;
export const SIDEBAR_WIDTH_MAX = 480;
export const SIDEBAR_WIDTH_DEFAULT = 240;

/** Composer send binding (SET.6). `"enter"`: Enter sends, Shift+Enter = new line.
 *  `"ctrlEnter"`: Ctrl/⌘+Enter sends, plain Enter = new line. */
export type SendMessageKey = "enter" | "ctrlEnter";
const SEND_MESSAGE_KEY_DEFAULT: SendMessageKey = "enter";

/** Factory default agent mode new sessions inherit (RFC 0011 §4, #266). Auto. */
const DEFAULT_MODE_DEFAULT: Mode = "auto";

export interface PrefsState {
  theme: Theme;
  font: Font;
  /** Root font scale, percent (see FONT_SCALE_MIN/MAX). */
  fontScale: number;
  /** Overrides the author name on sent messages; blank = system alias. */
  displayName: string;
  notifications: NotificationPrefs;
  /** Max threads kept loaded (LRU); FE flag until the backend consumes it. */
  openThreads: number;
  /** Composer send binding (Keyboard section, SET.6). */
  sendMessageKey: SendMessageKey;
  /** Agent mode new sessions inherit when no explicit mode is set (#266, RFC 0011). */
  defaultMode: Mode;
  /** Session sidebar collapsed to width 0 (#185). */
  sidebarCollapsed: boolean;
  /** Session sidebar width in px when expanded (#204); clamped to SIDEBAR_WIDTH_MIN/MAX. */
  sidebarWidth: number;
  setTheme: (theme: Theme) => void;
  setFont: (font: Font) => void;
  setFontScale: (scale: number) => void;
  setDisplayName: (name: string) => void;
  setNotifications: (partial: Partial<NotificationPrefs>) => void;
  setOpenThreads: (count: number) => void;
  setSendMessageKey: (key: SendMessageKey) => void;
  setDefaultMode: (mode: Mode) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setSidebarWidth: (px: number) => void;
  /** Quick light/dark flip — leaves `"system"` by picking the opposite effective mode. */
  toggleTheme: () => void;
  /** Reset only the Appearance-owned prefs to their defaults (SET.2 footer reset). */
  resetAppearance: () => void;
  /** Reset only the Keyboard-owned prefs to their defaults (SET.6 footer reset). */
  resetKeyboard: () => void;
}

/** Default values for the Appearance-owned prefs. Shared by initial state and
 *  `resetAppearance` so the footer reset and first-run agree. */
const APPEARANCE_DEFAULTS: Pick<
  PrefsState,
  | "theme"
  | "font"
  | "fontScale"
  | "displayName"
  | "notifications"
  | "openThreads"
> = {
  theme: "system",
  font: "geist",
  fontScale: 100,
  displayName: "",
  notifications: {
    enabled: true,
    messageComplete: true,
    approvalRequests: true,
    sound: false,
  },
  openThreads: 10,
};

function clampOpenThreads(count: number): number {
  return Math.min(
    OPEN_THREADS_MAX,
    Math.max(OPEN_THREADS_MIN, Math.round(count)),
  );
}

/** Clamp a sidebar width to its usable bounds. Exported for tests. */
export function clampSidebarWidth(px: number): number {
  return Math.min(
    SIDEBAR_WIDTH_MAX,
    Math.max(SIDEBAR_WIDTH_MIN, Math.round(px)),
  );
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
      ...APPEARANCE_DEFAULTS,
      // Keyboard-owned (SET.6) — kept out of APPEARANCE_DEFAULTS so the Appearance
      // reset doesn't touch it; `resetKeyboard` owns its reset.
      sendMessageKey: SEND_MESSAGE_KEY_DEFAULT,
      defaultMode: DEFAULT_MODE_DEFAULT,
      sidebarCollapsed: false,
      sidebarWidth: SIDEBAR_WIDTH_DEFAULT,
      setTheme: (theme) => set({ theme }),
      setFont: (font) => set({ font }),
      setFontScale: (scale) => set({ fontScale: clampFontScale(scale) }),
      setDisplayName: (displayName) => set({ displayName }),
      setNotifications: (partial) =>
        set((s) => ({ notifications: { ...s.notifications, ...partial } })),
      setOpenThreads: (count) => set({ openThreads: clampOpenThreads(count) }),
      setSendMessageKey: (sendMessageKey) => set({ sendMessageKey }),
      setDefaultMode: (defaultMode) => set({ defaultMode }),
      setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
      setSidebarWidth: (px) => set({ sidebarWidth: clampSidebarWidth(px) }),
      toggleTheme: () => {
        const { theme } = get();
        const effective = resolveEffectiveTheme(theme);
        set({ theme: effective === "light" ? "dark" : "light" });
      },
      resetAppearance: () => set({ ...APPEARANCE_DEFAULTS }),
      resetKeyboard: () =>
        set({
          sendMessageKey: SEND_MESSAGE_KEY_DEFAULT,
          defaultMode: DEFAULT_MODE_DEFAULT,
        }),
    }),
    {
      name: STORAGE_KEY,
      // `defaultMode` is deliberately NOT persisted (#287 review): P2 made the
      // backend `mode.json` the source of truth (`get_default_mode`). Persisting it
      // here too would let the stale localStorage value win on rehydration once the
      // IPC seam lands. It stays transient (Auto each launch) until that seam reads
      // it. Everything else persists as before.
      partialize: ({ defaultMode: _drop, ...rest }) => rest,
      // `current` (defaults) first so blobs persisted before SET.2 — which lack
      // the new keys — hydrate with sensible defaults rather than `undefined`.
      merge: (persisted, current) => ({
        ...current,
        ...(persisted as Partial<PrefsState>),
        ...migrateLegacyTheme(),
      }),
      onRehydrateStorage: () => (state) => {
        if (state) {
          applyTheme(state.theme);
          applyFont(state.font);
          applyFontScale(state.fontScale);
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

  const { theme, font, fontScale } = usePrefsStore.getState();
  applyTheme(theme);
  applyFont(font);
  applyFontScale(fontScale);
  setupSystemThemeListener(() => usePrefsStore.getState().theme);

  usePrefsStore.subscribe((state) => {
    applyTheme(state.theme);
    applyFont(state.font);
    applyFontScale(state.fontScale);
  });
}

/** @alias usePrefsStore — backward-compatible hook name for theme consumers. */
export { usePrefsStore as useTheme };
