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
  /** The user message id a session is currently editing in place (#463), or
   *  undefined when composing a fresh message. Drives the input bar's edit banner
   *  and routes submit to `editMessage` instead of `send`. */
  editingBySession: Record<string, string | undefined>;
  setText: (sessionId: string, text: string) => void;
  /** Load `text` into a session's composer and request focus (edit & resend). */
  prefill: (sessionId: string, text: string) => void;
  /** Enter in-place edit mode for `messageId` (#463): bind the session to it and
   *  load its `text` into the composer with focus. Unlike `prefill`, this is a
   *  targeted action so it sets the text directly (no #48 draft guard). */
  beginEdit: (sessionId: string, messageId: string, text: string) => void;
  /** Exit edit mode for a session and clear its composer (Cancel / Escape / after
   *  a submitted edit). */
  cancelEdit: (sessionId: string) => void;
}

export const useComposerStore = create<ComposerState>((set, get) => ({
  textBySession: {},
  focusNonceBySession: {},
  rejectNonceBySession: {},
  editingBySession: {},
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
  beginEdit: (sessionId, messageId, text) =>
    set((s) => ({
      editingBySession: { ...s.editingBySession, [sessionId]: messageId },
      textBySession: { ...s.textBySession, [sessionId]: text },
      focusNonceBySession: {
        ...s.focusNonceBySession,
        [sessionId]: (s.focusNonceBySession[sessionId] ?? 0) + 1,
      },
    })),
  cancelEdit: (sessionId) =>
    set((s) => ({
      editingBySession: { ...s.editingBySession, [sessionId]: undefined },
      textBySession: { ...s.textBySession, [sessionId]: "" },
    })),
}));
