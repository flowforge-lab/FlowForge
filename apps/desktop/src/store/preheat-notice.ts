// Holds the single active "phenotype preheat entries were dropped" notice (#1179),
// fed by the `phenotype:preheat-dropped` event (wired in lib/events.ts) and rendered
// by <PreheatDroppedToast>. One slot only, mirroring pheno-mcp-notice: phenotype is
// global in v1, so a fresh activation replaces any standing notice rather than
// stacking.
//
// No `seq` counter here (pheno-mcp-notice carries one for an auto-dismiss timer this
// toast deliberately does not have) — replacing the payload is enough to re-render.

import { create } from "zustand";
import type { PhenotypePreheatDroppedEvent } from "@/bindings";

interface PreheatNoticeState {
  notice: PhenotypePreheatDroppedEvent | null;
  show: (notice: PhenotypePreheatDroppedEvent) => void;
  dismiss: () => void;
}

export const usePreheatNoticeStore = create<PreheatNoticeState>((set) => ({
  notice: null,
  show: (notice) => set({ notice }),
  dismiss: () => set({ notice: null }),
}));
