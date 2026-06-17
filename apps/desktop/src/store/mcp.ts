// MCP server-status cache (#91, RFC 0003 §7). Mirrors store/skills.ts: a small
// zustand store fed by `listMcpServers` on load and replaced wholesale from the
// `mcp:status-changed` snapshot (wired in lib/events.ts). Action wrappers call the
// ipc.ts seam; the backend reconciles `mcp.json` and emits the new snapshot, so we
// don't mutate optimistically.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { McpServerConfig } from "@/bindings/McpServerConfig";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

interface McpState {
  servers: McpServerStatus[];
  loading: boolean;
  error: string | null;
  /** Fetch the current snapshot (on panel mount). */
  load: () => Promise<void>;
  /** Replace the snapshot from a `mcp:status-changed` event. */
  setServers: (servers: McpServerStatus[]) => void;
  restart: (id: string) => Promise<void>;
  setEnabled: (id: string, enabled: boolean) => Promise<void>;
  add: (def: McpServerConfig) => Promise<void>;
  remove: (id: string) => Promise<void>;
}

export const useMcpStore = create<McpState>((set) => ({
  servers: [],
  loading: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      set({ servers: await ipc.listMcpServers(), loading: false });
    } catch (e) {
      set({
        loading: false,
        error: e instanceof Error ? e.message : String(e),
      });
    }
  },

  setServers: (servers) => set({ servers }),

  restart: (id) => ipc.restartMcpServer(id),
  setEnabled: (id, enabled) => ipc.setMcpServerEnabled(id, enabled),
  add: (def) => ipc.addMcpServer(def),
  remove: (id) => ipc.removeMcpServer(id),
}));
