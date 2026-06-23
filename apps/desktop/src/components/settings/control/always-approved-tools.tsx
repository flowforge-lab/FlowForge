import { useEffect, useState } from "react";
import { ShieldCheck, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { useChatStore } from "@/store/chat";

/**
 * Always-Approved Tools (#229): the persistent allowlist behind "Always allow".
 * Unlike the presentation-only matrix/overrides above, this is the real runtime
 * set — the backend skips the approval prompt for every tool listed here. Reads
 * the chat store's mirror (refetched on mount) and revokes via `revokeAlways`.
 */
export function AlwaysApprovedTools() {
  const alwaysApproved = useChatStore((s) => s.alwaysApproved);
  const loadAlwaysApproved = useChatStore((s) => s.loadAlwaysApproved);
  const revokeAlways = useChatStore((s) => s.revokeAlways);
  const [loadError, setLoadError] = useState(false);

  useEffect(() => {
    void loadAlwaysApproved().then((ok) => setLoadError(!ok));
  }, [loadAlwaysApproved]);

  const tools = [...alwaysApproved].sort();

  return (
    <section className="space-y-2 border-t pt-5">
      <h3 className="flex items-center gap-1.5 text-[13px] font-medium text-foreground">
        <ShieldCheck className="size-3.5 text-emerald-600 dark:text-emerald-400" />
        Always-approved tools
      </h3>
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        Tools you chose to “Always allow” at an approval prompt. These run
        without asking, across every session. Revoke one to be prompted again.
      </p>
      {loadError ? (
        <p
          role="alert"
          className="rounded-lg border border-dashed border-destructive/40 px-3 py-4 text-center text-[12px] text-destructive"
        >
          Couldn’t load always-approved tools. Reopen Settings to retry.
        </p>
      ) : tools.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-[12px] text-muted-foreground">
          No tools are always-approved yet.
        </p>
      ) : (
        <ul className="flex flex-col gap-1">
          {tools.map((tool) => (
            <li
              key={tool}
              className="flex items-center gap-2 rounded-md bg-muted/50 px-2 py-1"
            >
              <code className="min-w-0 flex-1 truncate text-[11px] text-foreground">
                {tool}
              </code>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="text-muted-foreground hover:text-destructive"
                onClick={() => void revokeAlways(tool)}
                title={`Revoke ${tool}`}
                aria-label={`Revoke ${tool}`}
              >
                <X />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
