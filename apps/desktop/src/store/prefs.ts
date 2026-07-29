// Unified display preferences — theme, font, font scale, display name,
// notifications, open-thread budget. Persisted under `"ff-prefs"` via
// `durableStorage` (#1121, was plain localStorage); `subscribe` applies
// side-effects on change.
//
// `durableStorage` hydrates asynchronously, which would cost us the pre-paint
// theme/font script in `index.html` — it runs before any module loads and can
// only read something synchronous. `mirrorBootPrefs` therefore keeps a
// localStorage COPY of just theme+font for that script to read. The copy is a
// cache, never a source of truth: if a WKWebView drops its flush the worst case
// is one wrong-theme frame that hydration immediately corrects, not a lost pref.

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { durableStorage } from "@/lib/durable-storage";
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
import { ipc } from "@/lib/ipc";
import type { Mode } from "@/bindings";

const STORAGE_KEY = "ff-prefs";
const LEGACY_THEME_KEY = "ff-theme";

/** localStorage key holding the pre-paint theme/font cache read by the boot
 *  script in `index.html`. Deliberately NOT `STORAGE_KEY`: `durableStorage`
 *  adopts and clears any legacy localStorage value under a store's own key, so
 *  reusing it would make the cache look like real persisted state. */
export const BOOT_PREFS_KEY = "ff-boot-prefs";

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
  /** Agent mode new sessions inherit when no explicit mode is set (#266, RFC 0011).
   *  Source of truth is the backend `mode.json` (#798): hydrated from
   *  `getDefaultMode` at boot and written through on every change. Kept transient
   *  (not persisted) so the backend value always wins. */
  defaultMode: Mode;
  /** Session sidebar collapsed to width 0 (#185). */
  sidebarCollapsed: boolean;
  /** Session sidebar width in px when expanded (#204); clamped to SIDEBAR_WIDTH_MIN/MAX. */
  sidebarWidth: number;
  /** False until `durableStorage`'s (always-async) read has landed. Layout that
   *  would visibly jump when the persisted value arrives — the sidebar's width
   *  and collapsed state — waits on this instead of painting the default first.
   *  Runtime-only, never persisted. */
  hasHydrated: boolean;
  setTheme: (theme: Theme) => void;
  setFont: (font: Font) => void;
  setFontScale: (scale: number) => void;
  setDisplayName: (name: string) => void;
  setNotifications: (partial: Partial<NotificationPrefs>) => void;
  setOpenThreads: (count: number) => void;
  setSendMessageKey: (key: SendMessageKey) => void;
  /** Set the global default mode and write it through to the backend (`mode.json`). */
  setDefaultMode: (mode: Mode) => void;
  /** Pull the persisted default mode from the backend. Call once, gated on
   *  `app:ready`. Best-effort — leaves the `auto` default on failure. */
  hydrateDefaultMode: () => Promise<void>;
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

/** Refresh the pre-paint theme/font cache the `index.html` boot script reads.
 *  Best-effort and non-authoritative — see the module header. Exported for tests. */
export function mirrorBootPrefs(theme: Theme, font: Font): void {
  try {
    localStorage.setItem(BOOT_PREFS_KEY, JSON.stringify({ theme, font }));
  } catch {
    // localStorage unavailable (SSR/tests) — the app just starts on CSS defaults.
  }
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
      hasHydrated: false,
      setTheme: (theme) => set({ theme }),
      setFont: (font) => set({ font }),
      setFontScale: (scale) => set({ fontScale: clampFontScale(scale) }),
      setDisplayName: (displayName) => set({ displayName }),
      setNotifications: (partial) =>
        set((s) => ({ notifications: { ...s.notifications, ...partial } })),
      setOpenThreads: (count) => set({ openThreads: clampOpenThreads(count) }),
      setSendMessageKey: (sendMessageKey) => set({ sendMessageKey }),
      setDefaultMode: (defaultMode) => {
        set({ defaultMode });
        // Write through so the choice persists to `mode.json` (#798). The store
        // stays authoritative for the UI; this mirrors it to the backend, exactly
        // like session-mode's setMode mirrors to setSessionMode.
        void ipc.setDefaultMode(defaultMode);
      },
      hydrateDefaultMode: async () => {
        try {
          set({ defaultMode: await ipc.getDefaultMode() });
        } catch (e) {
          // Backend unreachable — keep the current (auto) default. Log so a real
          // `get_default_mode` failure is debuggable rather than a silent wrong value.
          console.warn("hydrateDefaultMode failed, keeping current default", e);
        }
      },
      setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
      setSidebarWidth: (px) => set({ sidebarWidth: clampSidebarWidth(px) }),
      toggleTheme: () => {
        const { theme } = get();
        const effective = resolveEffectiveTheme(theme);
        set({ theme: effective === "light" ? "dark" : "light" });
      },
      resetAppearance: () => set({ ...APPEARANCE_DEFAULTS }),
      resetKeyboard: () => {
        set({
          sendMessageKey: SEND_MESSAGE_KEY_DEFAULT,
          defaultMode: DEFAULT_MODE_DEFAULT,
        });
        // Persist the default-mode reset to the backend too (#798), so it survives
        // a relaunch rather than being reverted by the next hydrate.
        void ipc.setDefaultMode(DEFAULT_MODE_DEFAULT);
      },
    }),
    {
      name: STORAGE_KEY,
      storage: createJSONStorage(() => durableStorage),
      // `defaultMode` is deliberately NOT persisted (#287 review): P2 made the
      // backend `mode.json` the source of truth (`get_default_mode`). Persisting it
      // here too would let the stale localStorage value win on rehydration once the
      // IPC seam lands. It stays transient (Auto each launch) until that seam reads
      // it. Everything else persists as before.
      // `hasHydrated` is a runtime signal, not a preference — it always starts
      // `false` in memory each launch regardless of what was persisted.
      partialize: ({ defaultMode: _drop, hasHydrated: _drop2, ...rest }) =>
        rest,
      // `current` (defaults) first so blobs persisted before SET.2 — which lack
      // the new keys — hydrate with sensible defaults rather than `undefined`.
      merge: (persisted, current) => ({
        ...current,
        ...(persisted as Partial<PrefsState>),
        ...migrateLegacyTheme(),
      }),
      // Fires once the (async) read lands, whether it resolved to a real value,
      // `null` (fresh install), or failed (logged in `durable-storage.ts`).
      // `hasHydrated` must flip in every one of those cases, or the sidebar
      // would never paint.
      onRehydrateStorage: () => (state) => {
        if (state) {
          applyTheme(state.theme);
          applyFont(state.font);
          applyFontScale(state.fontScale);
          mirrorBootPrefs(state.theme, state.font);
        }
        usePrefsStore.setState({ hasHydrated: true });
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
    // Keep the pre-paint cache in step with every change, so the next launch
    // paints the theme the user last chose rather than the one before it.
    mirrorBootPrefs(state.theme, state.font);
  });
}

/** @alias usePrefsStore — backward-compatible hook name for theme consumers. */
export { usePrefsStore as useTheme };
