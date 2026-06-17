// Per-session composer state (Issue #18, re-keyed for split panes #148). Lets a
// message row's "edit & resend" prefill the input bar and refocus it without
// prop-drilling. With split panes each session has its own draft, so the text /
// focus / reject nonces are keyed by sessionId — typing in one pane never leaks
// into another. The composer text is the single source of truth here;
// `focusNonce` bumps on every prefill so the input bar can imperatively focus +
// grow the textarea even when the text is unchanged.
// Mirrors the small single-purpose stores in store/split.ts and store/palette.ts.

import { create } from "zustand";

interface ComposerState {
  /** Current composer text per session — the input bar reads this as its value. */
  textBySession: Record<string, string>;
  /** Incremented on each prefill so a refocus fires even for identical text. */
  focusNonceBySession: Record<string, number>;
  /** Incremented when a prefill is refused so it didn't clobber a draft (#48),
   *  so the input bar can flash the preserved draft. */
  rejectNonceBySession: Record<string, number>;
  setText: (sessionId: string, text: string) => void;
  /** Load `text` into a session's composer and request focus (edit & resend). */
  prefill: (sessionId: string, text: string) => void;
}

export const useComposerStore = create<ComposerState>((set, get) => ({
  textBySession: {},
  focusNonceBySession: {},
  rejectNonceBySession: {},
  setText: (sessionId, text) =>
    set((s) => ({
      textBySession: { ...s.textBySession, [sessionId]: text },
    })),
  prefill: (sessionId, text) => {
    // Don't silently clobber an in-progress draft (#48). If the composer already
    // has content, refuse and bump rejectNonce (the input bar flashes the draft);
    // the draft is preserved, nothing destroyed. An empty/whitespace composer
    // prefills as before.
    const current = get().textBySession[sessionId] ?? "";
    if (current.trim().length > 0) {
      set((s) => ({
        rejectNonceBySession: {
          ...s.rejectNonceBySession,
          [sessionId]: (s.rejectNonceBySession[sessionId] ?? 0) + 1,
        },
      }));
      return;
    }
    set((s) => ({
      textBySession: { ...s.textBySession, [sessionId]: text },
      focusNonceBySession: {
        ...s.focusNonceBySession,
        [sessionId]: (s.focusNonceBySession[sessionId] ?? 0) + 1,
      },
    }));
  },
}));
