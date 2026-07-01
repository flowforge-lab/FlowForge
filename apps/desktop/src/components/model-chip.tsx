import { useEffect } from "react";
import { Check, ChevronDown, Cpu } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import { useModelConfigStore, isLocalKind } from "@/store/model-config";
import { useProfilesStore } from "@/store/profiles";
import { useSessionModelStore } from "@/store/session-model";
import { ModelWindowInfo } from "@/components/model-window-info";

// Per-pane model chip (RFC 0005 §11.4, Phase D; #499). Shows the resolved model
// for this session and opens a quick picker (connection → model) that writes a
// session-scoped override; clearing falls back to the phenotype/global tiers.
// Session-scoped (like ModePill / WorkspaceSelector / ContextGauge), so each
// tiling pane (#148) selects independently. The backend resolves authoritatively
// (session > phenotype > global); this only reflects + sets it.
export function ModelChip({ sessionId }: { sessionId: string }) {
  const registry = useModelConfigStore((s) => s.registry);
  const modelsById = useModelConfigStore((s) => s.modelsById);
  const loadModels = useModelConfigStore((s) => s.loadModels);
  const loadRegistry = useModelConfigStore((s) => s.load);
  const registryLoading = useModelConfigStore((s) => s.loading);
  // Inline reasoning toggle for local models (#633): reads/writes the resolved
  // connection's `thinking` via the existing per-connection action — no new IPC.
  const setThinking = useModelConfigStore((s) => s.setThinking);
  const saving = useModelConfigStore((s) => s.saving);

  // Resolution depends on the global active connection + the active phenotype, so
  // re-resolve when either changes (the picker's own set/clear reloads directly).
  const phenoActiveId = useProfilesStore((s) => s.activeId);

  const resolved = useSessionModelStore((s) => s.resolvedBySession[sessionId]);
  const override = useSessionModelStore(
    (s) => s.overrideBySession[sessionId] ?? null,
  );
  const unavailable = useSessionModelStore(
    (s) => s.unavailableBySession[sessionId] ?? false,
  );
  // Served context window + source for this session (#602). Absent until the
  // backend forwards it; drives the dropdown readout and the under-fill warning dot.
  const servedWindow = useSessionModelStore(
    (s) => s.servedWindowBySession[sessionId],
  );
  const load = useSessionModelStore((s) => s.load);
  const setSelection = useSessionModelStore((s) => s.set);
  const clear = useSessionModelStore((s) => s.clear);

  // Hydrate the registry once (Settings also loads it, but the composer shouldn't
  // depend on Settings being opened). The shared store means only the first pane
  // triggers the fetch.
  useEffect(() => {
    if (!registry && !registryLoading) void loadRegistry();
  }, [registry, registryLoading, loadRegistry]);

  useEffect(() => {
    void load(sessionId);
  }, [sessionId, load, registry?.active, phenoActiveId]);

  // The backend resolver is unavailable for this session (e.g. the Phase D commands
  // aren't registered yet — this FE can merge ahead of its backend half). Hide the
  // chip entirely rather than show a permanently spinning / non-functional control;
  // it reappears once `load` succeeds. All hooks above run unconditionally first.
  if (unavailable) return null;

  const connections = registry?.connections ?? [];
  const resolvedConn = connections.find((c) => c.id === resolved?.connection);

  // Lazily fetch each connection's model list when the picker opens.
  const onOpenChange = (open: boolean) => {
    if (!open) return;
    for (const c of connections) {
      if (!modelsById[c.id]) void loadModels(c.id);
    }
  };

  return (
    <DropdownMenu onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          title={
            resolved
              ? `Model: ${resolvedConn?.displayName ?? resolved.connection} · ${resolved.model}${override ? " (session override)" : " (inherited)"}`
              : "Model"
          }
          aria-label="Session model"
          className="h-6 max-w-[40%] gap-1 px-1.5 text-xs font-medium text-muted-foreground hover:text-foreground"
        >
          <Cpu className="size-3.5 shrink-0" />
          {resolved ? (
            <span className="min-w-0 truncate">{resolved.model}</span>
          ) : (
            <Spinner className="shrink-0" />
          )}
          {/* A dot marks a session-scoped override vs an inherited selection. */}
          {override ? (
            <span
              aria-hidden
              className="size-1.5 shrink-0 rounded-full bg-primary"
            />
          ) : null}
          {/* Always-visible under-fill warning (#602): the served window fell back to
              the conservative default — likely a mis-set OLLAMA_CONTEXT_LENGTH — so
              flag it here rather than letting it pass silently. */}
          {servedWindow?.source === "default" ? (
            <span
              role="img"
              className="size-1.5 shrink-0 rounded-full bg-amber-500"
              title="Context window not detected — using the conservative default. Set OLLAMA_CONTEXT_LENGTH or FLOWFORGE_OLLAMA_NUM_CTX."
              aria-label="Context window not detected — using the conservative default"
            />
          ) : null}
          <ChevronDown className="size-3 shrink-0 opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-48">
        {/* Served context window + source (#602), shown above the picker so the
            user can see what window the active model is actually serving. */}
        {servedWindow ? (
          <>
            <div className="px-2 py-1.5">
              <ModelWindowInfo info={servedWindow} />
            </div>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {/* Inline "Thinking" toggle for local models (#633) — surfaces the existing
            per-connection reasoning switch (also in Settings → Model) right where the
            model is picked, so the speed↔reasoning tradeoff on CPU is one click away.
            `onSelect`-preventDefault keeps the menu open; the Switch owns the change. */}
        {resolvedConn && isLocalKind(resolvedConn.kind) ? (
          <>
            <DropdownMenuItem
              onSelect={(e) => e.preventDefault()}
              className="flex items-start justify-between gap-3"
            >
              <span className="flex min-w-0 flex-col">
                <span className="text-[13px] font-medium text-foreground">
                  Thinking
                </span>
                <span className="text-[11px] leading-snug text-muted-foreground">
                  Off is faster on local models; on for hard tasks.
                </span>
              </span>
              <Switch
                aria-label="Thinking"
                checked={resolvedConn.thinking}
                disabled={saving}
                onCheckedChange={(v) => void setThinking(resolvedConn.id, v)}
              />
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {connections.length === 0 ? (
          <DropdownMenuItem disabled>
            {registryLoading ? "Loading…" : "No connections"}
          </DropdownMenuItem>
        ) : (
          connections.map((c) => {
            const models = modelsById[c.id];
            return (
              <DropdownMenuSub key={c.id}>
                <DropdownMenuSubTrigger>
                  <Check
                    className={cn(
                      c.id === resolved?.connection
                        ? "opacity-100"
                        : "opacity-0",
                    )}
                  />
                  <span className="min-w-0 truncate">{c.displayName}</span>
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent className="max-h-72 max-w-64 overflow-y-auto">
                  {models === undefined ? (
                    <DropdownMenuItem disabled>Loading…</DropdownMenuItem>
                  ) : models.length === 0 ? (
                    <DropdownMenuItem disabled>No models</DropdownMenuItem>
                  ) : (
                    models.map((m) => (
                      <DropdownMenuItem
                        key={m}
                        onSelect={() =>
                          void setSelection(sessionId, {
                            connection: c.id,
                            model: m,
                          })
                        }
                      >
                        <Check
                          className={cn(
                            c.id === resolved?.connection &&
                              m === resolved?.model
                              ? "opacity-100"
                              : "opacity-0",
                          )}
                        />
                        <span className="min-w-0 truncate">{m}</span>
                      </DropdownMenuItem>
                    ))
                  )}
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            );
          })
        )}
        <DropdownMenuSeparator />
        {/* Clearing the override returns the pane to the phenotype/global tiers. */}
        <DropdownMenuItem
          disabled={!override}
          onSelect={() => void clear(sessionId)}
        >
          <span className="text-muted-foreground">
            Use phenotype / global default
          </span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
