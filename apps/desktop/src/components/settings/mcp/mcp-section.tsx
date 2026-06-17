import { useEffect } from "react";
import { ServerRow } from "@/components/settings/mcp/server-row";
import { AddServerForm } from "@/components/settings/mcp/add-server-form";
import { useMcpStore } from "@/store/mcp";

/**
 * MCP servers section (#91, RFC 0003 §7). Lists configured servers with live
 * status (badge + tool count + last error) and per-server actions; live updates
 * arrive via `mcp:status-changed` (wired in lib/events.ts).
 */
export function McpSection() {
  const servers = useMcpStore((s) => s.servers);
  const loading = useMcpStore((s) => s.loading);
  const error = useMcpStore((s) => s.error);
  const load = useMcpStore((s) => s.load);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="space-y-4">
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        External tool servers from <code className="text-[11px]">mcp.json</code>
        . Enable, disable, restart, or add definitions; changes reconcile the
        running supervisor.
      </p>

      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}

      {loading && servers.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">Loading…</p>
      ) : servers.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">
          No MCP servers configured.
        </p>
      ) : (
        <div className="space-y-2">
          {servers.map((server) => (
            <ServerRow key={server.id} server={server} />
          ))}
        </div>
      )}

      <AddServerForm />
    </div>
  );
}
