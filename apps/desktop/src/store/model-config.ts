// Model section state (#126, extended for the provider-card surface in #202 PR-3b).
// The provider registry (connections + active pointer) is durable backend config,
// loaded from / persisted via IPC; reasoning effort is now a per-connection field on
// it (#395), set like `thinking` via `upsertConnection`. The summarization threshold
// still has no backend field, so it persists locally under `ff-model`; `partialize`
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

/** Locally-persisted reasoning controls without a backend field yet. Effort moved
 *  to a per-connection backend field in #395; only the threshold remains local. */
interface LocalReasoningPrefs {
  summarizationThreshold: number;
}

const LOCAL_REASONING_DEFAULTS: LocalReasoningPrefs = {
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
    reasoningEffort: "medium",
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

  /** Set a connection's reasoning effort (per-connection, IPC-backed like
   *  `thinking`; the active connection drives the global reasoning controls). */
  setEffort: (id: string, effort: Effort) => Promise<void>;
  setSummarizationThreshold: (tokens: number) => void;
  /** Reset the local reasoning controls to defaults (footer reset). Effort is now
   *  backend-owned (per-connection), so reset leaves it untouched — only the local
   *  summarization threshold returns to default. */
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

        setEffort: async (id, effort) => {
          const conn = get().registry?.connections.find((c) => c.id === id);
          if (!conn || conn.reasoningEffort === effort) return;
          set({ saving: true, error: null });
          try {
            await ipc.upsertConnection({ ...conn, reasoningEffort: effort });
            await refresh();
            set({ saving: false });
          } catch (err) {
            set({ saving: false, error: errMsg(err) });
          }
        },
        setSummarizationThreshold: (tokens) =>
          set({ summarizationThreshold: clampThreshold(tokens) }),

        resetModel: () => set({ ...LOCAL_REASONING_DEFAULTS }),
      };
    },
    {
      name: "ff-model",
      // Only the summarization threshold persists locally; the registry (incl.
      // per-connection effort, #395) is backend-owned.
      partialize: (s) => ({
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

/** Canonicalize a provider base URL before it's stored. The provider client
 *  appends `/models` and `/chat/completions` itself, so a base URL that already
 *  ends in `/chat/completions` (a common copy-paste from SiliconFlow/OpenAI curl
 *  docs) yields `…/chat/completions/models` → 404. Trim whitespace, drop trailing
 *  slashes, and strip a trailing `/chat/completions` (or `/chat/completions/`).
 *  Returns "" for blank input (callers map that to the default endpoint). */
export function normalizeBaseUrl(input: string): string {
  let url = input.trim();
  if (!url) return "";
  url = url.replace(/\/+$/, "");
  url = url.replace(/\/chat\/completions$/i, "");
  url = url.replace(/\/+$/, "");
  return url;
}

/** Hosted, OpenAI-compatible providers authenticated with a single bearer API key
 *  over an editable base URL (vs. Bedrock's region/auth form or the local kinds'
 *  plain host field). Drives the provider-card credentials layout. */
export function isHostedKeyKind(kind: ProviderKind): boolean {
  return kind === "openai" || kind === "siliconFlow";
}

/** Kinds whose backend chat request sends no reasoning-*enable* parameter, so the
 *  "Thinking" toggle has no wire effect — flipping it changes nothing the server
 *  acts on (reasoning still streams when the model emits `reasoning_content` /
 *  `reasoning`). Kept distinct from [`isHostedKeyKind`] (credentials shape): the
 *  two sets coincide today but answer different questions. */
export function reasoningToggleNoOp(kind: ProviderKind): boolean {
  return kind === "openai" || kind === "siliconFlow";
}
