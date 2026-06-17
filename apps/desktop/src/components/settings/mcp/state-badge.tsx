import { cn } from "@/lib/utils";
import { mcpStateMeta } from "@/lib/mcp";
import type { McpServerState } from "@/bindings/McpServerState";

/** Small status pill — leading dot + label, styled per state with FlowForge tokens. */
export function StateBadge({ state }: { state: McpServerState }) {
  const meta = mcpStateMeta(state);
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px] font-medium",
        meta.badgeClassName,
      )}
    >
      <span
        className={cn("size-1.5 rounded-full", meta.dotClassName)}
        aria-hidden
      />
      {meta.label}
    </span>
  );
}
