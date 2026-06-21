// Model section state (#126, extended for the provider-card surface in #202 PR-3b).
// The provider registry (connections + active pointer) is durable backend config,
// loaded from / persisted via IPC. Effort / summarization threshold have no backend
// field yet, so they persist locally under `ff-model` until one lands; `partialize`
// keeps the IPC-backed cache out of localStorage.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ipc } from "@/lib/ipc";
import type { ProviderConnection } from "@/bindings/ProviderConnection";
import type { ProviderRegistry } from "@/bindings/ProviderRegistry";
import type { ProviderKind } from "@/bindings/ProviderKind";
import type { SecretKind } from "@/bindings/SecretKind";

export type Effort = "low" | "medium" | "high";

/** Per-connection "Test Connection" outcome (#202 PR-3a). */
export type TestState =
  | { state: "loading" }
  | { state: "ok" }
  | { state: "error"; message: string };

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

/** A fresh, unconfigured connection for a kind the user is adding. The backend
 *  derives the real `id` on upsert; secrets are added separately, write-only. */
function defaultConnection(kind: ProviderKind): ProviderConnection {
  const base: ProviderConnection = {
    id: "",
    kind,
    displayName: "",
    model: "",
    hasKey: false,
    thinking: true,
    supportsVision: false,
  };
  switch (kind) {
    case "bedrock":
      return {
        ...base,
        displayName: "AWS Bedrock",
        region: "us-east-1",
        authMode: "profile",
      };
    case "ollama":
      return { ...base, displayName: "Ollama" };
    case "candleVllm":
      return { ...base, displayName: "candle-vLLM" };
    case "openai":
      return { ...base, displayName: "OpenAI" };
    case "siliconFlow":
      return { ...base, displayName: "SiliconFlow" };
  }
}

interface ModelConfigState extends LocalReasoningPrefs {
  /** Durable provider registry (IPC-backed); `null` until first load. */
  registry: ProviderRegistry | null;
  /** Best-effort model ids per connection id (lazy; populated on load/expand). */
  modelsById: Record<string, string[]>;
  /** In-flight "Test Connection" results, keyed by connection id. */
  test: Record<string, TestState>;
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  /** Best-effort refresh of one connection's model list (e.g. on card expand). */
  loadModels: (id: string) => Promise<void>;
  /** Make a connection the default (active) provider. */
  setActiveConnection: (id: string) => Promise<void>;
  /** Pick a connection's default model — also makes that connection active. */
  setDefaultModel: (id: string, model: string) => Promise<void>;
  /** Persist non-secret connection edits (region / auth mode / profile / etc.). */
  saveConnection: (conn: ProviderConnection) => Promise<ProviderConnection>;
  /** Add a fresh connection of `kind`, make it active, and return its stored id. */
  addConnection: (kind: ProviderKind) => Promise<string>;
  removeConnection: (id: string) => Promise<void>;
  /** Toggle reasoning streams for a connection (the active one drives the global
   *  reasoning controls). */
  setThinking: (id: string, thinking: boolean) => Promise<void>;
  /** Store a secret (write-only); `hasKey` updates from the refreshed registry. */
  setSecret: (id: string, kind: SecretKind, value: string) => Promise<void>;
  /** Clear a stored secret; `hasKey` updates from the refreshed registry. */
  clearSecret: (id: string, kind: SecretKind) => Promise<void>;
  /** Probe a connection; records loading / ok / error under `test[id]`. */
  testConnection: (id: string) => Promise<void>;
  /** Drop a connection's test result (e.g. when the user edits creds again). */
  clearTest: (id: string) => void;

  setEffort: (effort: Effort) => void;
  setSummarizationThreshold: (tokens: number) => void;
  /** Reset the local reasoning controls to defaults (footer reset). The durable
   *  registry is backend-owned and left untouched. */
  resetModel: () => void;
}

function clampThreshold(tokens: number): number {
  return Math.min(
    SUMMARY_THRESHOLD_MAX,
    Math.max(SUMMARY_THRESHOLD_MIN, Math.round(tokens)),
  );
}

const errMsg = (err: unknown): string =>
  err instanceof Error ? err.message : String(err);

export const useModelConfigStore = create<ModelConfigState>()(
  persist(
    (set, get) => {
      /** Re-pull the registry after a mutation so derived state (hasKey, active,
       *  model) always reflects backend truth rather than an optimistic guess. */
      const refresh = async () => {
        const registry = await ipc.getProviderRegistry();
        set({ registry });
        return registry;
      };

      return {
        ...LOCAL_REASONING_DEFAULTS,
        registry: null,
        modelsById: {},
        test: {},
        loading: false,
        saving: false,
        error: null,

        load: async () => {
          set({ loading: true, error: null });
          try {
            const registry = await ipc.getProviderRegistry();
            const models = await ipc.listModels(registry.active);
            set({
              registry,
              modelsById: { [registry.active]: models },
              loading: false,
            });
          } catch (err) {
            set({ loading: false, error: errMsg(err) });
          }
        },

        loadModels: async (id) => {
          try {
            const models = await ipc.listModels(id);
            set((s) => ({ modelsById: { ...s.modelsById, [id]: models } }));
          } catch {
            // Best-effort: an unreachable endpoint just leaves the list empty.
            set((s) => ({ modelsById: { ...s.modelsById, [id]: [] } }));
          }
        },

        setActiveConnection: async (id) => {
          if (get().registry?.active === id) return;
          set({ saving: true, error: null });
          try {
            await ipc.setActiveConnection(id);
            await refresh();
            await get().loadModels(id);
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },

        setDefaultModel: async (id, model) => {
          const conn = get().registry?.connections.find((c) => c.id === id);
          if (!conn) return;
          set({ saving: true, error: null });
          try {
            if (get().registry?.active !== id) {
              await ipc.setActiveConnection(id);
            }
            if (conn.model !== model) {
              await ipc.upsertConnection({ ...conn, model });
            }
            await refresh();
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },

        saveConnection: async (conn) => {
          set({ saving: true, error: null });
          try {
            const stored = await ipc.upsertConnection(conn);
            await refresh();
            set({ saving: false });
            return stored;
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
            throw err;
          }
        },

        addConnection: async (kind) => {
          set({ saving: true, error: null });
          try {
            const stored = await ipc.upsertConnection(defaultConnection(kind));
            await ipc.setActiveConnection(stored.id);
            await refresh();
            await get().loadModels(stored.id);
            set({ saving: false });
            return stored.id;
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
            throw err;
          }
        },

        removeConnection: async (id) => {
          set({ saving: true, error: null });
          try {
            await ipc.removeConnection(id);
            const registry = await refresh();
            await get().loadModels(registry.active);
            set((s) => {
              const { [id]: _t, ...test } = s.test;
              const { [id]: _m, ...modelsById } = s.modelsById;
              return { saving: false, test, modelsById };
            });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },

        setThinking: async (id, thinking) => {
          const conn = get().registry?.connections.find((c) => c.id === id);
          if (!conn || conn.thinking === thinking) return;
          set({ saving: true, error: null });
          try {
            await ipc.upsertConnection({ ...conn, thinking });
            await refresh();
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },

        setSecret: async (id, kind, value) => {
          set({ saving: true, error: null });
          try {
            await ipc.setProviderSecret(id, kind, value);
            await refresh();
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
            throw err;
          }
        },

        clearSecret: async (id, kind) => {
          set({ saving: true, error: null });
          try {
            await ipc.clearProviderSecret(id, kind);
            await refresh();
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },

        testConnection: async (id) => {
          set((s) => ({ test: { ...s.test, [id]: { state: "loading" } } }));
          try {
            await ipc.testConnection(id);
            set((s) => ({ test: { ...s.test, [id]: { state: "ok" } } }));
          } catch (err) {
            set((s) => ({
              test: {
                ...s.test,
                [id]: { state: "error", message: errMsg(err) },
              },
            }));
          }
        },

        clearTest: (id) =>
          set((s) => {
            const { [id]: _gone, ...rest } = s.test;
            return { test: rest };
          }),

        setEffort: (effort) => set({ effort }),
        setSummarizationThreshold: (tokens) =>
          set({ summarizationThreshold: clampThreshold(tokens) }),

        resetModel: () => set({ ...LOCAL_REASONING_DEFAULTS }),
      };
    },
    {
      name: "ff-model",
      // Only effort/threshold persist locally; the registry is backend-owned.
      partialize: (s) => ({
        effort: s.effort,
        summarizationThreshold: s.summarizationThreshold,
      }),
    },
  ),
);

/** The active (default) connection, or `null` before load. */
export function activeConnection(
  registry: ProviderRegistry | null,
): ProviderConnection | null {
  if (!registry) return null;
  return registry.connections.find((c) => c.id === registry.active) ?? null;
}
