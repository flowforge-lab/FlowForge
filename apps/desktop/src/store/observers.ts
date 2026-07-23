// Active background observers per session (#1038 FE, epic #954 M2). A standalone
// Zustand slice — like `processes.ts` — that backs the `👁 Observers` panel.
//
// Unlike `processes.ts` (which is purely push-based off `process:output` /
// `process:exited`), observers are a *command + event hybrid*: the set is read
// with `list_observers(sessionId)` and mutated with `stop_observer(id,
// sessionId)`, and the backend also emits a coarse `observer:changed` whenever
// the active set changes (start / stop / fire) so the panel re-lists without a
// manual refresh. State is transient runtime data — never persisted (observers
// don't survive a relaunch; the backend reaps them on session close).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { ObserverInfo } from "@/bindings";

interface ObserversState {
  /** sessionId -> its active observers (oldest id first, as `list_observers`
   *  returns them). A session with no observers is absent from the record,
   *  which is what the panel keys its self-hide on. */
  bySession: Record<string, ObserverInfo[]>;

  /** Fetch `sessionId`'s active observers and replace its cached list. Called
   *  on panel mount and whenever `observer:changed` fires for the session. */
  load: (sessionId: string) => Promise<void>;
  /** Alias of {@link load}; the name the `observer:changed` handler uses. */
  refresh: (sessionId: string) => Promise<void>;
  /** Stop one observer (the panel's `[×]`), then reload so the row clears
   *  immediately (the `observer:changed` the stop emits also reloads). */
  stop: (id: number, sessionId: string) => Promise<void>;
  /** Drop a session's observers (no IPC). Used by the chat store's
   *  delete-session reconciliation so a vanished session doesn't dangle. */
  clear: (sessionId: string) => void;
}

export const useObserversStore = create<ObserversState>((set, get) => ({
  bySession: {},

  load: async (sessionId) => {
    const list = await ipc.listObservers(sessionId);
    set((s) => ({ bySession: { ...s.bySession, [sessionId]: list } }));
  },

  refresh: (sessionId) => get().load(sessionId),

  stop: async (id, sessionId) => {
    await ipc.stopObserver(id, sessionId);
    await get().load(sessionId);
  },

  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.bySession)) return s;
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    }),
}));
