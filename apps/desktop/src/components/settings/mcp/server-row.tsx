import { Power, RotateCcw, Trash2 } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { StateBadge } from "@/components/settings/mcp/state-badge";
import { useMcpStore } from "@/store/mcp";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

/** One MCP server: id, state badge, tool count, last error, and actions. */
export function ServerRow({ server }: { server: McpServerStatus }) {
  const restart = useMcpStore((s) => s.restart);
  const setEnabled = useMcpStore((s) => s.setEnabled);
  const remove = useMcpStore((s) => s.remove);

  const disabled = server.state === "disabled";
  const toolLabel = `${server.toolCount} ${server.toolCount === 1 ? "tool" : "tools"}`;

  return (
    <div className="rounded-lg border border-border bg-card px-3 py-2.5">
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-foreground">
          {server.id}
          {server.scopeKey ? (
            <span
              className="ml-1.5 font-normal text-[11px] text-muted-foreground"
              title={`Workspace scope: ${server.scopeKey}`}
            >
              {server.scopeKey}
            </span>
          ) : null}
        </span>
        <StateBadge state={server.state} />
      </div>

      <div className="mt-1 flex items-center gap-2 text-[11px] text-muted-foreground">
        <span>{toolLabel}</span>
        {server.restarts > 0 ? (
          <span>
            · {server.restarts} restart{server.restarts === 1 ? "" : "s"}
          </span>
        ) : null}
      </div>

      {(server.state === "failed" || server.state === "restarting") &&
      server.lastError ? (
        <p
          className="mt-1.5 text-[11px] leading-relaxed text-destructive"
          role="alert"
        >
          {server.lastError}
        </p>
      ) : null}

      <div className="mt-2.5 flex items-center gap-1.5">
        <Button
          variant="outline"
          size="xs"
          onClick={() => void restart(server.id)}
          disabled={disabled}
          title={
            disabled ? "Enable the server to restart it" : "Restart server"
          }
        >
          <RotateCcw />
          Restart
        </Button>
        <Button
          variant="outline"
          size="xs"
          onClick={() => void setEnabled(server.id, disabled)}
        >
          <Power />
          {disabled ? "Enable" : "Disable"}
        </Button>
        <Button
          variant="ghost"
          size="xs"
          className="ml-auto text-muted-foreground hover:text-destructive"
          onClick={() => void remove(server.id)}
          title="Remove definition"
        >
          <Trash2 />
          Remove
        </Button>
      </div>
    </div>
  );
}
