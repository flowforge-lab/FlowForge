// Shared composer state (Issue #18). Lets a message row's "edit & resend" prefill
// the input bar and refocus it without prop-drilling. The composer text is the
// single source of truth here; `focusNonce` bumps on every prefill so the input
// bar can imperatively focus + grow the textarea even when the text is unchanged.
// Mirrors the small single-purpose stores in store/split.ts and store/palette.ts.

import { create } from "zustand";

interface ComposerState {
  /** Current composer text — the input bar reads this as its value. */
  text: string;
  /** Incremented on each prefill so a refocus fires even for identical text. */
  focusNonce: number;
  /** Incremented when a prefill is refused so it didn't clobber a draft (#48),
   *  so the input bar can flash the preserved draft. */
  rejectNonce: number;
  setText: (text: string) => void;
  /** Load `text` into the composer and request focus (edit & resend). */
  prefill: (text: string) => void;
}

export const useComposerStore = create<ComposerState>((set, get) => ({
  text: "",
  focusNonce: 0,
  rejectNonce: 0,
  setText: (text) => set({ text }),
  prefill: (text) => {
    // Don't silently clobber an in-progress draft (#48). If the composer already
    // has content, refuse and bump rejectNonce (the input bar flashes the draft);
    // the draft is preserved, nothing destroyed. An empty/whitespace composer
    // prefills as before.
    if (get().text.trim().length > 0) {
      set({ rejectNonce: get().rejectNonce + 1 });
      return;
    }
    set({ text, focusNonce: get().focusNonce + 1 });
  },
}));
