// Web-search settings for the settings panel (#84). Loaded from / persisted via
// IPC — the backend owns durable storage; this store is the UI cache.

import { create } from "zustand";
import type { SearchBackend, SearchConfig } from "@/bindings";
import { ipc } from "@/lib/ipc";

interface SearchConfigState {
  config: SearchConfig | null;
  loading: boolean;
  saving: boolean;
  error: string | null;
  load: () => Promise<void>;
  setBackend: (backend: SearchBackend) => Promise<void>;
  setBaseUrl: (baseUrl: string) => Promise<void>;
}

export const useSearchConfigStore = create<SearchConfigState>((set, get) => ({
  config: null,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const config = await ipc.getSearchConfig();
      set({ config, loading: false });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setBackend: async (backend) => {
    const { config } = get();
    if (!config) return;
    set({ saving: true, error: null });
    try {
      const baseUrl = backend === "searxNg" ? config.baseUrl : undefined;
      const stored = await ipc.setSearchConfig(backend, baseUrl);
      set({ config: stored, saving: false });
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setBaseUrl: async (baseUrl) => {
    const { config } = get();
    if (!config || config.backend !== "searxNg") return;
    set({ saving: true, error: null });
    try {
      const stored = await ipc.setSearchConfig("searxNg", baseUrl);
      set({ config: stored, saving: false });
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },
}));
