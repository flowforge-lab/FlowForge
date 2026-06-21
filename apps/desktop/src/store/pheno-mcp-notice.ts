// Holds the single active "phenotype skill needs an unavailable MCP server"
// notice (#301), fed by the `phenotype:mcp-unavailable` event (wired in
// lib/events.ts) and rendered by <PhenoMcpToast>. One slot only: phenotype is
// global in v1, so a fresh activation replaces any standing notice rather than
// stacking. `seq` bumps on every show so the toast re-arms its auto-dismiss timer
// even when one notice replaces another with the same content.

import { create } from "zustand";
import type { PhenotypeMcpUnavailableEvent } from "@/lib/phenotype-mcp";

interface PhenoMcpNoticeState {
  notice: PhenotypeMcpUnavailableEvent | null;
  seq: number;
  show: (notice: PhenotypeMcpUnavailableEvent) => void;
  dismiss: () => void;
}

export const usePhenoMcpNoticeStore = create<PhenoMcpNoticeState>((set) => ({
  notice: null,
  seq: 0,
  show: (notice) => set((s) => ({ notice, seq: s.seq + 1 })),
  dismiss: () => set({ notice: null }),
}));
