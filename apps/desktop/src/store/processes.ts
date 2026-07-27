// Live output of background processes started via `process_manager` (#873 FE,
// #987). A standalone Zustand slice — like `notebook.ts` / `goal.ts` — pushed to
// by the `process:output` / `process:exited` backend events (wired once in
// `lib/events.ts`), not polled.
//
// Unlike tool output (`toolStepsByMessage`, keyed by `messageId` and scoped to
// one turn), a background process keeps streaming across turns for its whole
// life, so its output lives here keyed by `sessionId` + `processId` with no turn
// affinity. The output buffer is append-only; `status` stays `null` while
// running and becomes the supervisor's terminal label once `process:exited`
// lands.

import { create } from "zustand";
import type { ProcessOutputEvent, ProcessExitedEvent } from "@/bindings";

/** One background process's live view. `output` is the append-only stdout+stderr
 *  tail; `status` is `null` while running, then the terminal label from
 *  `process:exited` (`"exited(0)"`, `"killed"`, `"failed: <reason>"`, …). */
export interface ProcessState {
  processId: number;
  output: string;
  status: string | null;
  startedAt: number;
}

interface ProcessesState {
  /** sessionId -> (processId -> live process view). A session with no
   *  background processes is simply absent from the record, which is what the
   *  status panel keys its self-hide on. */
  bySession: Record<string, Record<number, ProcessState>>;

  /** Append a `process:output` chunk to its process, materializing the process
   *  on its first chunk. Unlike `applyToolOutputChunk`, an unknown id is NOT
   *  dropped: a process's first chunk has no pre-existing step to attach to —
   *  the chunk itself is what brings the process into being. */
  applyProcessOutput: (e: ProcessOutputEvent) => void;
  /** Flip a process to its terminal `status`. Materializes the process if the
   *  exit somehow arrives before any output (an empty-output process). */
  applyProcessExited: (e: ProcessExitedEvent) => void;
  /** Drop a session's processes (no IPC). Used by the chat store's delete-session
   *  reconciliation so a vanished session's buffers don't dangle in memory. */
  clear: (sessionId: string) => void;
  /** Drop one *finished* process's buffer (#1089). No IPC: the process has
   *  already exited, so this is pure view cleanup — nothing to stop backend-side
   *  (contrast the observer panel's `[×]`, which calls `stop_observer`). The UI
   *  only offers this on terminal rows; dropping a running process's buffer
   *  would orphan output that is still arriving. Empties the session key when it
   *  removes the last entry, so the panel self-hides exactly as after `clear`. */
  dismiss: (sessionId: string, processId: number) => void;
  /** Drop every terminal process for a session in one go (#1089) — the bulk form
   *  of `dismiss`, for when a long session has accumulated a stack of exited
   *  rows. Running processes are left alone. Same no-IPC and drop-empty-session
   *  rules as `dismiss`. */
  clearFinished: (sessionId: string) => void;
}

export const useProcessesStore = create<ProcessesState>((set) => ({
  bySession: {},

  applyProcessOutput: (e) =>
    set((s) => {
      const forSession = s.bySession[e.sessionId] ?? {};
      const prev = forSession[e.processId];
      const next: ProcessState = prev
        ? { ...prev, output: prev.output + e.delta }
        : {
            processId: e.processId,
            output: e.delta,
            status: null,
            startedAt: Date.now(),
          };
      return {
        bySession: {
          ...s.bySession,
          [e.sessionId]: { ...forSession, [e.processId]: next },
        },
      };
    }),

  applyProcessExited: (e) =>
    set((s) => {
      const forSession = s.bySession[e.sessionId] ?? {};
      const prev = forSession[e.processId];
      const next: ProcessState = prev
        ? { ...prev, status: e.status }
        : {
            processId: e.processId,
            output: "",
            status: e.status,
            startedAt: Date.now(),
          };
      return {
        bySession: {
          ...s.bySession,
          [e.sessionId]: { ...forSession, [e.processId]: next },
        },
      };
    }),

  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.bySession)) return s;
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    }),

  dismiss: (sessionId, processId) =>
    set((s) => {
      const forSession = s.bySession[sessionId];
      if (!forSession || !(processId in forSession)) return s;
      const { [processId]: _dropped, ...remaining } = forSession;
      // Last one out drops the session key, which is what the panel's
      // `if (!byId) return null` self-hide keys on.
      if (Object.keys(remaining).length === 0) {
        const { [sessionId]: _empty, ...rest } = s.bySession;
        return { bySession: rest };
      }
      return { bySession: { ...s.bySession, [sessionId]: remaining } };
    }),

  clearFinished: (sessionId) =>
    set((s) => {
      const forSession = s.bySession[sessionId];
      if (!forSession) return s;
      const running = Object.entries(forSession).filter(
        ([, p]) => p.status === null,
      );
      // Nothing terminal — return the state untouched rather than a new object,
      // so a no-op call can't re-render the panel.
      if (running.length === Object.keys(forSession).length) return s;
      if (running.length === 0) {
        const { [sessionId]: _empty, ...rest } = s.bySession;
        return { bySession: rest };
      }
      return {
        bySession: {
          ...s.bySession,
          [sessionId]: Object.fromEntries(running),
        },
      };
    }),
}));
