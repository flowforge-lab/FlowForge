import { useEffect, useState } from "react";
import { Check, ChevronDown, Dna } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { DEFAULT_PROFILE_ID, useProfilesStore } from "@/store/profiles";
import { useChatStore } from "@/store/chat";
import { ipc } from "@/lib/ipc";

// Phenotype selector for a session-pane header (#245 2b, per-session #935).
// Picks the working set THIS pane's session runs as. Session-scoped like
// `ModePill` / `WorkspaceSelector` / `ModelChip`: the binding is written per
// session via `ipc.setSessionPhenotype`, so two panes can run different
// phenotypes. A session with no binding inherits the global active phenotype
// (`useProfilesStore.activeId`) — untouched panes track the last-used global
// choice. The catalog (`profiles`) stays global; only the selection is per pane.
export function PhenoSelector({ sessionId }: { sessionId: string }) {
  const profiles = useProfilesStore((s) => s.profiles);
  const activeId = useProfilesStore((s) => s.activeId);
  const loading = useProfilesStore((s) => s.loading);
  const load = useProfilesStore((s) => s.load);
  const loadError = useProfilesStore((s) => s.error);

  const bound = useChatStore(
    (s) => s.sessions.find((x) => x.id === sessionId)?.phenotype,
  );
  const patchSessionPhenotype = useChatStore((s) => s.patchSessionPhenotype);

  // Per-pane switch state (do NOT use the global `useProfilesStore.saving`,
  // which would spin every pane). `error` here is a failed switch on this pane.
  const [saving, setSaving] = useState(false);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const error = switchError ?? loadError;

  // Lazily hydrate the list the first time a pane mounts (the Profiles settings
  // section also loads it, but the header shouldn't depend on Settings being
  // opened). The shared store means only the first pane triggers the fetch. The
  // `!error` guard stops a failed load from re-firing in a loop (load clears
  // `loading` on rejection, leaving the list empty); the error is surfaced below.
  useEffect(() => {
    if (profiles.length === 0 && !loading && !loadError) void load();
  }, [profiles.length, loading, loadError, load]);

  // The session's active phenotype: its own binding, else the global active.
  const resolvedId = bound ?? activeId;
  const active = profiles.find((p) => p.id === resolvedId);
  // The built-in `default` is a hidden immutable fallback, not a user choice
  // (#935): show only the on-disk phenotypes in the picker.
  const items = profiles.filter((p) => p.id !== DEFAULT_PROFILE_ID);

  const setPhenotype = async (name: string) => {
    if (name === resolvedId) return;
    // Optimistic: reflect the choice immediately, revert if the write rejects.
    setSaving(true);
    setSwitchError(null);
    patchSessionPhenotype(sessionId, name);
    try {
      await ipc.setSessionPhenotype(sessionId, name);
      setSaving(false);
    } catch (err) {
      patchSessionPhenotype(sessionId, bound ?? null);
      setSaving(false);
      setSwitchError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          disabled={saving}
          title="Session phenotype"
          aria-label="Session phenotype"
          className="h-6 max-w-[45%] gap-1 px-1.5 text-xs font-medium text-muted-foreground hover:text-foreground"
        >
          {saving ? (
            <Spinner className="shrink-0" />
          ) : (
            <Dna className="size-3.5 shrink-0" />
          )}
          <span className="min-w-0 truncate">
            {active?.name ?? "Phenotype"}
          </span>
          <ChevronDown className="size-3 shrink-0 opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-44">
        {items.length === 0 ? (
          // Loading / error / empty are distinct: a failed IPC must not read as a
          // clean cold-start. `error` carries the message in a tooltip.
          <DropdownMenuItem disabled title={error ?? undefined}>
            {loading ? "Loading…" : error ? "Failed to load" : "No phenotypes"}
          </DropdownMenuItem>
        ) : (
          items.map((p) => (
            <DropdownMenuItem
              key={p.id}
              onSelect={() => void setPhenotype(p.id)}
            >
              <Check
                className={cn(
                  p.id === resolvedId ? "opacity-100" : "opacity-0",
                )}
              />
              <span className="min-w-0 truncate">{p.name}</span>
            </DropdownMenuItem>
          ))
        )}
        <div
          role="presentation"
          className="border-t px-2 py-1.5 text-[11px] text-muted-foreground/80"
        >
          Applies to this pane
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
