// Model section state (#126). The provider (kind + model) is durable backend
// config — loaded from / persisted via IPC, mirroring store/search-config.ts. The
// reasoning controls (thinking / effort / summarization threshold) have no backend
// field yet, so they persist locally under `ff-model` until one lands; `partialize`
// keeps the IPC-backed cache out of localStorage.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ipc } from "@/lib/ipc";
import type { ProviderConfig } from "@/bindings/ProviderConfig";
import type { ProviderKind } from "@/bindings/ProviderKind";

export type Effort = "low" | "medium" | "high";

/** Bounds for the summarization threshold (tokens). */
export const SUMMARY_THRESHOLD_MIN = 50_000;
export const SUMMARY_THRESHOLD_MAX = 300_000;

/** Locally-persisted reasoning controls and their first-run defaults. */
interface ReasoningPrefs {
  thinking: boolean;
  effort: Effort;
  summarizationThreshold: number;
}

const REASONING_DEFAULTS: ReasoningPrefs = {
  thinking: true,
  effort: "medium",
  summarizationThreshold: 150_000,
};

interface ModelConfigState extends ReasoningPrefs {
  /** Durable provider config (IPC-backed); `null` until first load. */
  provider: ProviderConfig | null;
  /** Best-effort model ids for the active connection. */
  models: string[];
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setKind: (kind: ProviderKind) => Promise<void>;
  setModel: (model: string) => Promise<void>;
  setThinking: (thinking: boolean) => void;
  setEffort: (effort: Effort) => void;
  setSummarizationThreshold: (tokens: number) => void;
  /** Reset the local reasoning controls to defaults (footer reset). The durable
   *  provider is backend-owned and left untouched. */
  resetModel: () => void;
}

function clampThreshold(tokens: number): number {
  return Math.min(
    SUMMARY_THRESHOLD_MAX,
    Math.max(SUMMARY_THRESHOLD_MIN, Math.round(tokens)),
  );
}

export const useModelConfigStore = create<ModelConfigState>()(
  persist(
    (set, get) => ({
      ...REASONING_DEFAULTS,
      provider: null,
      models: [],
      loading: false,
      saving: false,
      error: null,

      load: async () => {
        set({ loading: true, error: null });
        try {
          const [provider, models] = await Promise.all([
            ipc.getProviderConfig(),
            ipc.listModels(),
          ]);
          set({ provider, models, loading: false });
        } catch (err) {
          set({
            loading: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      },

      setKind: async (kind) => {
        const { provider } = get();
        if (!provider || provider.kind === kind) return;
        set({ saving: true, error: null });
        try {
          const stored = await ipc.setProviderConfig(
            kind,
            provider.baseUrl,
            provider.model,
          );
          // The active connection's model list can differ per kind.
          const models = await ipc.listModels();
          set({ provider: stored, models, saving: false });
        } catch (err) {
          set({
            saving: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      },

      setModel: async (model) => {
        const { provider } = get();
        if (!provider || provider.model === model) return;
        set({ saving: true, error: null });
        try {
          const stored = await ipc.setProviderConfig(
            provider.kind,
            provider.baseUrl,
            model,
          );
          set({ provider: stored, saving: false });
        } catch (err) {
          set({
            saving: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      },

      setThinking: (thinking) => set({ thinking }),
      setEffort: (effort) => set({ effort }),
      setSummarizationThreshold: (tokens) =>
        set({ summarizationThreshold: clampThreshold(tokens) }),

      resetModel: () => set({ ...REASONING_DEFAULTS }),
    }),
    {
      name: "ff-model",
      // Only the reasoning controls persist; the provider/model cache is owned by
      // the backend and re-fetched on load.
      partialize: (s) => ({
        thinking: s.thinking,
        effort: s.effort,
        summarizationThreshold: s.summarizationThreshold,
      }),
    },
  ),
);
