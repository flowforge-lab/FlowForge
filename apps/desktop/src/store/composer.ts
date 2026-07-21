// Per-session composer state (Issue #18, re-keyed for split panes #148). Holds each
// session's draft text and staged attachments without prop-drilling. With split panes
// each session has its own draft, so the text / focus / reject nonces are keyed by
// sessionId — typing in one pane never leaks into another. The composer text is the
// single source of truth here; `focusNonce` bumps on every prefill so the input bar
// can imperatively focus + grow the textarea even when the text is unchanged.
//
// Editing a past message no longer routes through here (#929): it happens in a
// bubble-anchored box on the message itself (`message-edit-box.tsx`), which is what
// removed the old `editingBySession` / `beginEdit` / `cancelEdit` seam. The composer
// is send-only again.
// Mirrors the small single-purpose stores in store/split.ts and store/palette.ts.

import { create } from "zustand";

import type { Attachment } from "@/bindings";

interface ComposerState {
  /** Current composer text per session — the input bar reads this as its value. */
  textBySession: Record<string, string>;
  /** Staged attachments per session (#723). Lifted out of the input bar's local
   *  state so a region-wide, pane-level drop (session-pane.tsx) can stage into the
   *  right composer; the input bar renders the chips and clears them on submit. */
  attachmentsBySession: Record<string, Attachment[]>;
  /** Incremented on each prefill so a refocus fires even for identical text. */
  focusNonceBySession: Record<string, number>;
  /** Incremented when a prefill is refused so it didn't clobber a draft (#48),
   *  so the input bar can flash the preserved draft. */
  rejectNonceBySession: Record<string, number>;
  setText: (sessionId: string, text: string) => void;
  /** Append a staged attachment for a session (drop / paste / pick). */
  stageAttachment: (sessionId: string, attachment: Attachment) => void;
  /** Remove one staged attachment by index (chip ✕). */
  removeAttachment: (sessionId: string, index: number) => void;
  /** Drop all staged attachments for a session (on submit). */
  clearAttachments: (sessionId: string) => void;
  /** Load `text` into a session's composer and request focus (edit & resend). */
  prefill: (sessionId: string, text: string) => void;
}

export const useComposerStore = create<ComposerState>((set, get) => ({
  textBySession: {},
  attachmentsBySession: {},
  focusNonceBySession: {},
  rejectNonceBySession: {},
  setText: (sessionId, text) =>
    set((s) => ({
      textBySession: { ...s.textBySession, [sessionId]: text },
    })),
  stageAttachment: (sessionId, attachment) =>
    set((s) => ({
      attachmentsBySession: {
        ...s.attachmentsBySession,
        [sessionId]: [...(s.attachmentsBySession[sessionId] ?? []), attachment],
      },
    })),
  removeAttachment: (sessionId, index) =>
    set((s) => ({
      attachmentsBySession: {
        ...s.attachmentsBySession,
        [sessionId]: (s.attachmentsBySession[sessionId] ?? []).filter(
          (_, i) => i !== index,
        ),
      },
    })),
  clearAttachments: (sessionId) =>
    set((s) => ({
      attachmentsBySession: { ...s.attachmentsBySession, [sessionId]: [] },
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
