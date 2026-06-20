import { useEffect } from "react";
import { Check, ChevronDown, Dna, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { useProfilesStore } from "@/store/profiles";

// Phenotype selector for a session-pane header (#245 2b). Establishes the UI
// location for "spin off a session as a phenotype" — picking the working set a
// session runs as.
//
// v1 caveat (intentional): phenotype is still GLOBAL. `useProfilesStore` holds a
// single `activeId`, so switching here calls `switchPhenotype` app-wide — every
// pane reflects the choice. True per-session binding is the backend follow-up in
// #246; this component is the seam that work will plug into.
export function PhenoSelector() {
  const profiles = useProfilesStore((s) => s.profiles);
  const activeId = useProfilesStore((s) => s.activeId);
  const loading = useProfilesStore((s) => s.loading);
  const saving = useProfilesStore((s) => s.saving);
  const error = useProfilesStore((s) => s.error);
  const load = useProfilesStore((s) => s.load);
  const setActive = useProfilesStore((s) => s.setActive);

  // Lazily hydrate the list the first time a pane mounts (the Profiles settings
  // section also loads it, but the header shouldn't depend on Settings being
  // opened). The shared store means only the first pane triggers the fetch. The
  // `!error` guard stops a failed load from re-firing in a loop (load clears
  // `loading` on rejection, leaving the list empty); the error is surfaced below.
  useEffect(() => {
    if (profiles.length === 0 && !loading && !error) void load();
  }, [profiles.length, loading, error, load]);

  const active = profiles.find((p) => p.id === activeId);

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
            <Loader2 className="size-3.5 shrink-0 animate-spin" />
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
        {profiles.length === 0 ? (
          // Loading / error / empty are distinct: a failed IPC must not read as a
          // clean cold-start. `error` carries the message in a tooltip.
          <DropdownMenuItem disabled title={error ?? undefined}>
            {loading ? "Loading…" : error ? "Failed to load" : "No phenotypes"}
          </DropdownMenuItem>
        ) : (
          profiles.map((p) => (
            <DropdownMenuItem key={p.id} onSelect={() => void setActive(p.id)}>
              <Check
                className={cn(p.id === activeId ? "opacity-100" : "opacity-0")}
              />
              <span className="min-w-0 truncate">{p.name}</span>
            </DropdownMenuItem>
          ))
        )}
        <div
          role="presentation"
          className="border-t px-2 py-1.5 text-[11px] text-muted-foreground/80"
        >
          Applies to all panes (v1)
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
