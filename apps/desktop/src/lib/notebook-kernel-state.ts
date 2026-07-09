// `NotebookKernelState` — FE-owned for now (#871 FE-1). There is no backend
// Rust type or ts-rs binding yet; the `notebook_status` / `notebook_stop`
// Tauri commands this shape backs don't exist either (they land in a
// follow-up BE PR). Kept as a plain hand-written type outside `bindings/`
// rather than a fake-generated file there — a file in `bindings/` carrying
// the ts-rs "do not edit" header implies it's regenerated from a real Rust
// type, which this isn't (see #154/#159 on that drift class). Swap the import
// to the real generated binding once the backend PR lands ts-rs support for
// this shape; the field names below are chosen to match the proposed Rust
// struct so that swap is a no-op at every call site.
//
// Mirrors the canonical `kernel <id> — <running|dead>; pid=…; cells
// executed=…` line parsed by `lib/notebook-output.ts::parseKernelStatus`, but
// as a typed struct so the FE never has to re-parse the kernel-state text.
export type NotebookKernelState = {
  sessionId: string;
  /** False when the session has no kernel yet (`notebook_runner start` not
   *  called, or it was stopped / died). True means `state`, `kernelId`, and
   *  `pid` carry values; `executionCount` is always set. */
  hasKernel: boolean;
  /** Backend-derived state. Always set when `hasKernel`; null otherwise.
   *  `"dead"` reflects a kernel that died on its own (the tool sets it when
   *  it detects EOF on the kernel's pipe) — a user-initiated Stop removes the
   *  kernel entirely rather than tombstoning it as dead, so `notebookStop`
   *  collapses `hasKernel` to `false`, it never produces `"dead"`. */
  state: "running" | "dead" | null;
  /** Backend-assigned kernel id (e.g. `kernel-abcd1234`). Null when no kernel. */
  kernelId: string | null;
  /** Backend process pid. Null when no kernel or unavailable. */
  pid: number | null;
  /** Number of cells executed so far in the kernel's lifetime, sourced from
   *  the canonical status line's `cells executed=N` token. Zero when the
   *  kernel exists but no cell has run yet. */
  executionCount: number;
  /** Canonical status line the backend emits — kept verbatim so the FE can
   *  fall back to the existing `parseKernelStatus` view if a future backend
   *  change widens the shape. */
  raw: string;
};
