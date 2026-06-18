// Model section state (#126). The provider (kind + model + thinking) is durable
// backend config — loaded from / persisted via IPC, mirroring store/search-config.ts.
// Effort / summarization threshold have no backend field yet, so they persist
// locally under `ff-model` until one lands; `partialize` keeps the IPC-backed
// cache out of localStorage.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ipc } from "@/lib/ipc";
import type { ProviderConfig } from "@/bindings/ProviderConfig";
import type { ProviderKind } from "@/bindings/ProviderKind";

export type Effort = "low" | "medium" | "high";

/** Bounds for the summarization threshold (tokens). */
export const SUMMARY_THRESHOLD_MIN = 50_000;
export const SUMMARY_THRESHOLD_MAX = 300_000;

/** Locally-persisted reasoning controls without a backend field yet. */
interface LocalReasoningPrefs {
  effort: Effort;
  summarizationThreshold: number;
}

const LOCAL_REASONING_DEFAULTS: LocalReasoningPrefs = {
  effort: "medium",
  summarizationThreshold: 150_000,
};

interface ModelConfigState extends LocalReasoningPrefs {
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
  setThinking: (thinking: boolean) => Promise<void>;
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
      ...LOCAL_REASONING_DEFAULTS,
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

      // Switching kind selects the seeded connection of that kind as active —
      // it does NOT push the current connection's baseUrl/model into the new
      // kind. Mutating in place leaked the prior provider's model name across
      // kinds (e.g. candle-vLLM's "Qwen3-4B-Instruct-2507" showing under
      // Ollama); flipping the active connection keeps each one's own config.
      setKind: async (kind) => {
        const { provider } = get();
        if (!provider || provider.kind === kind) return;
        set({ saving: true, error: null });
        try {
          const registry = await ipc.getProviderRegistry();
          const target = registry.connections.find((c) => c.kind === kind);
          if (!target) {
            throw new Error(`No ${kind} connection is configured.`);
          }
          await ipc.setActiveConnection(target.id);
          // Pull the now-active connection's own config + model list (the stored
          // config carries each connection's own `thinking` flag).
          const [stored, models] = await Promise.all([
            ipc.getProviderConfig(),
            ipc.listModels(),
          ]);
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
            provider.thinking,
          );
          set({ provider: stored, saving: false });
        } catch (err) {
          set({
            saving: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      },

      setThinking: async (thinking) => {
        const { provider } = get();
        if (!provider || provider.thinking === thinking) return;
        set({ saving: true, error: null });
        try {
          const stored = await ipc.setProviderConfig(
            provider.kind,
            provider.baseUrl,
            provider.model,
            thinking,
          );
          set({ provider: stored, saving: false });
        } catch (err) {
          set({
            saving: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      },

      setEffort: (effort) => set({ effort }),
      setSummarizationThreshold: (tokens) =>
        set({ summarizationThreshold: clampThreshold(tokens) }),

      resetModel: () => set({ ...LOCAL_REASONING_DEFAULTS }),
    }),
    {
      name: "ff-model",
      // Only effort/threshold persist locally; provider/thinking are backend-owned.
      partialize: (s) => ({
        effort: s.effort,
        summarizationThreshold: s.summarizationThreshold,
      }),
    },
  ),
);
