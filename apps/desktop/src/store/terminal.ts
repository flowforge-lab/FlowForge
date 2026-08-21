// Embedded terminal drawer state (#1284). Backs the bottom drawer each session
// pane can open, and the tab strip inside it.
//
// Scoped per session, like `file-panel.ts`: `openSessions` tracks which sessions
// have the drawer open, and `bySession` holds that session's ordered tabs plus
// which one is active, so two panes showing different sessions keep independent
// shells. The drawer height is one global value shared by every pane, exactly as
// the file panel's two divider widths are.
//
// What is *not* here: the shells themselves. A tab holds a `terminalId` handed
// out by the backend, and the xterm instance lives in the component that renders
// it. This store never touches a PTY -- the one exception is `closeTab`, which
// must kill the shell it is dropping, because nothing else would (contrast the
// processes store, whose `dismiss` is pure view cleanup over already-dead
// processes).
//
// Persistence mirrors `file-panel.ts`: open/height only, through `durableStorage`
// (#1134), since a WKWebView does not reliably flush localStorage before exit.
// Terminal *ids* are deliberately never persisted -- they name OS processes that
// died with the app, so a restored drawer opens fresh shells instead of pointing
// at ghosts.

import { create } from "zustand";
import { readDurable, writeDurable } from "@/lib/durable-json";
import { ipc } from "@/lib/ipc";

const STORAGE_KEY = "ff-terminal";

/** Drawer height bounds (px). Clamped so a drag can leave neither the drawer nor
 *  the transcript above it unusable. */
export const MIN_DRAWER_HEIGHT = 120;
export const MAX_DRAWER_HEIGHT = 640;
export const DEFAULT_DRAWER_HEIGHT = 260;

export function clampDrawerHeight(px: number): number {
  return Math.max(
    MIN_DRAWER_HEIGHT,
    Math.min(MAX_DRAWER_HEIGHT, Math.round(px)),
  );
}

/** One tab in a pane's drawer.
 *
 *  `terminalId` is `null` between the tab appearing and `terminal_open`
 *  resolving: the tab is rendered immediately so the strip never flickers, and
 *  the id lands a tick later. `exited` marks a shell that ended on its own
 *  (`exit`, or a crash) -- the tab stays until the user closes it, so its final
 *  output remains readable. */
export interface TerminalTab {
  /** Stable key for React and for addressing the tab; independent of the
   *  backend's terminal id, which does not exist yet at creation time. */
  tabId: string;
  terminalId: string | null;
  /** What the tab strip calls this shell, alongside the workspace folder. Every
   *  tab in a pane is rooted at the same directory, so the folder alone cannot
   *  tell two of them apart (#1287 review) -- the number is what does.
   *
   *  Per session, and never reused while its siblings are alive: closing shell 2
   *  of three leaves 1 and 3, rather than renumbering the tab the user is
   *  looking at out from under them. */
  shellNumber: number;
  exited: boolean;
}

interface SessionTerminals {
  tabs: TerminalTab[];
  /** `tabId` of the visible tab, or null when the session has no tabs. */
  activeTabId: string | null;
}

interface Persisted {
  openSessions: string[];
  drawerHeight: number;
}

const FALLBACK: Persisted = {
  openSessions: [],
  drawerHeight: DEFAULT_DRAWER_HEIGHT,
};

function parsePersisted(raw: unknown): Persisted {
  const p = (raw ?? {}) as Partial<Persisted>;
  return {
    openSessions: Array.isArray(p.openSessions)
      ? p.openSessions.filter((x): x is string => typeof x === "string")
      : [],
    drawerHeight: clampDrawerHeight(
      typeof p.drawerHeight === "number"
        ? p.drawerHeight
        : DEFAULT_DRAWER_HEIGHT,
    ),
  };
}

let nextTabId = 1;
function newTab(siblings: TerminalTab[]): TerminalTab {
  return {
    tabId: `tab-${nextTabId++}`,
    terminalId: null,
    // One past the highest live number, so numbers stay unique among the tabs
    // actually on screen without ever renumbering an existing one.
    shellNumber:
      siblings.reduce((max, t) => Math.max(max, t.shellNumber), 0) + 1,
    exited: false,
  };
}

interface TerminalState {
  /** Sessions with the drawer open. */
  openSessions: Set<string>;
  /** Per-session tabs. Absent = that session has never opened the drawer. */
  bySession: Record<string, SessionTerminals>;
  /** Drawer height (px), shared across panes. */
  drawerHeight: number;
  /** False until the async durable read lands. `session-pane.tsx` gates the
   *  drawer on it so a restored drawer doesn't snap in after first paint. */
  hasHydrated: boolean;

  /** Toggle the drawer for `sessionId`, seeding a first tab on the way open so
   *  opening it always lands on a live shell rather than an empty strip. */
  toggleDrawer: (sessionId: string) => void;
  /** Close the drawer without touching its tabs — the shells keep running, so
   *  re-opening returns to the same session state, which is the whole point of a
   *  drawer rather than a modal. */
  closeDrawer: (sessionId: string) => void;
  /** Add a tab (the `＋` button) and focus it. */
  addTab: (sessionId: string) => void;
  /** Focus a tab. */
  selectTab: (sessionId: string, tabId: string) => void;
  /** Close one tab, killing its shell. Closing the last one also closes the
   *  drawer: an empty strip with nothing in it is not a state worth showing. */
  closeTab: (sessionId: string, tabId: string) => void;
  /** Record the backend id for a tab once `terminal_open` resolves. */
  bindTerminal: (sessionId: string, tabId: string, terminalId: string) => void;
  /** Apply `terminal:exited`: mark the matching tab dead wherever it lives. A
   *  terminal id the store no longer knows is ignored — closing a tab kills its
   *  shell, so the event races the close that caused it. */
  applyExited: (terminalId: string) => void;
  /** Kill every shell for a session and forget it (session deleted). */
  clearSession: (sessionId: string) => void;
  /** Commit the drawer height after a drag. */
  setDrawerHeight: (px: number) => void;
  /** Adopt the persisted drawer state. Fired once on module load; exported so
   *  tests can re-run it after seeding storage. */
  hydrate: () => Promise<void>;
}

export const useTerminalStore = create<TerminalState>((set, get) => {
  const save = () => {
    // Before hydration the state is still defaults; writing them would clobber
    // the drawers the user actually left open.
    if (!get().hasHydrated) return;
    const s = get();
    writeDurable(STORAGE_KEY, {
      openSessions: [...s.openSessions],
      drawerHeight: s.drawerHeight,
    } satisfies Persisted);
  };

  /** Replace `sessionId`'s slice via `update`, leaving other sessions alone. */
  const patch = (
    sessionId: string,
    update: (slice: SessionTerminals) => SessionTerminals,
  ) => {
    const current = get().bySession[sessionId] ?? {
      tabs: [],
      activeTabId: null,
    };
    set((s) => ({
      bySession: { ...s.bySession, [sessionId]: update(current) },
    }));
  };

  const appendTab = (slice: SessionTerminals): SessionTerminals => {
    const tab = newTab(slice.tabs);
    return { tabs: [...slice.tabs, tab], activeTabId: tab.tabId };
  };

  return {
    openSessions: new Set<string>(),
    bySession: {},
    drawerHeight: DEFAULT_DRAWER_HEIGHT,
    hasHydrated: false,

    toggleDrawer: (sessionId) => {
      const openSessions = new Set(get().openSessions);
      if (openSessions.has(sessionId)) {
        openSessions.delete(sessionId);
        set({ openSessions });
      } else {
        openSessions.add(sessionId);
        set({ openSessions });
        const slice = get().bySession[sessionId];
        if (!slice || slice.tabs.length === 0) {
          patch(sessionId, appendTab);
        }
      }
      save();
    },

    closeDrawer: (sessionId) => {
      if (!get().openSessions.has(sessionId)) return;
      const openSessions = new Set(get().openSessions);
      openSessions.delete(sessionId);
      set({ openSessions });
      save();
    },

    addTab: (sessionId) => patch(sessionId, appendTab),

    selectTab: (sessionId, tabId) =>
      patch(sessionId, (slice) =>
        slice.tabs.some((t) => t.tabId === tabId)
          ? { ...slice, activeTabId: tabId }
          : slice,
      ),

    closeTab: (sessionId, tabId) => {
      const slice = get().bySession[sessionId];
      if (!slice) return;
      const closing = slice.tabs.find((t) => t.tabId === tabId);
      if (!closing) return;
      // Kill the shell. Unmounting the view would do it too, but only for the
      // tab that happens to be visible — an inactive tab is still mounted (its
      // shell must keep running), so its close has to be explicit.
      if (closing.terminalId) void ipc.closeTerminal(closing.terminalId);

      const tabs = slice.tabs.filter((t) => t.tabId !== tabId);
      // Focus the neighbour, preferring the one to the left, so closing a run of
      // tabs walks backwards instead of jumping to the end each time.
      const removedAt = slice.tabs.findIndex((t) => t.tabId === tabId);
      const activeTabId =
        slice.activeTabId !== tabId
          ? slice.activeTabId
          : ((tabs[removedAt - 1] ?? tabs[removedAt])?.tabId ?? null);
      set((s) => ({
        bySession: { ...s.bySession, [sessionId]: { tabs, activeTabId } },
      }));
      if (tabs.length === 0) get().closeDrawer(sessionId);
    },

    bindTerminal: (sessionId, tabId, terminalId) =>
      patch(sessionId, (slice) => ({
        ...slice,
        tabs: slice.tabs.map((t) =>
          t.tabId === tabId ? { ...t, terminalId } : t,
        ),
      })),

    applyExited: (terminalId) =>
      set((s) => {
        const bySession = { ...s.bySession };
        let changed = false;
        for (const [id, slice] of Object.entries(bySession)) {
          if (
            !slice.tabs.some((t) => t.terminalId === terminalId && !t.exited)
          ) {
            continue;
          }
          bySession[id] = {
            ...slice,
            tabs: slice.tabs.map((t) =>
              t.terminalId === terminalId ? { ...t, exited: true } : t,
            ),
          };
          changed = true;
        }
        // Returning the existing state object lets zustand skip the re-render
        // for the common case: an exit for a tab we already dropped.
        return changed ? { bySession } : s;
      }),

    clearSession: (sessionId) => {
      const slice = get().bySession[sessionId];
      if (slice) {
        for (const tab of slice.tabs) {
          if (tab.terminalId) void ipc.closeTerminal(tab.terminalId);
        }
      }
      const openSessions = new Set(get().openSessions);
      openSessions.delete(sessionId);
      set((s) => {
        const { [sessionId]: _dropped, ...bySession } = s.bySession;
        return { bySession, openSessions };
      });
      save();
    },

    setDrawerHeight: (px) => {
      set({ drawerHeight: clampDrawerHeight(px) });
      save();
    },

    hydrate: async () => {
      const stored = await readDurable(STORAGE_KEY, parsePersisted, FALLBACK);
      set((s) => {
        // Drawers opened while the read was in flight are newer than disk, so
        // they survive the merge rather than closing under the user.
        const openSessions = new Set([
          ...stored.openSessions,
          ...s.openSessions,
        ]);
        const bySession = { ...s.bySession };
        // A restored session has no tabs yet (ids are never persisted); seed one
        // so the drawer comes back with a live shell instead of an empty strip.
        for (const id of stored.openSessions) {
          if (!bySession[id] || bySession[id].tabs.length === 0) {
            bySession[id] = appendTab({ tabs: [], activeTabId: null });
          }
        }
        return {
          openSessions,
          bySession,
          drawerHeight: stored.drawerHeight,
          hasHydrated: true,
        };
      });
    },
  };
});

void useTerminalStore.getState().hydrate();
