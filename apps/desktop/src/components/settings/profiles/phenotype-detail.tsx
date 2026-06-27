import { useEffect, type ReactNode } from "react";
import {
  Check,
  ChevronDown,
  Copy,
  Cpu,
  Lock,
  Server,
} from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import { useModelConfigStore } from "@/store/model-config";
import { DEFAULT_PROFILE_ID, useProfilesStore } from "@/store/profiles";

// Phenotype detail/editor panel (RFC 0005 Phase D / #530), shown below the Installed
// grid for the selected phenotype. Binds the phenotype's Provider (a connection) and
// Model — its tier of the three-tier model resolution (session > phenotype > global) —
// reusing the model-chip connection→model picker pattern. Each row writes the whole
// `Phenotype` back via `update_phenotype` (lossless). The built-in `default` is
// immutable: its rows render read-only and offer "Duplicate to customize" instead,
// so the backend's reject path is never hit.
export function PhenotypeDetail({ phenotypeId }: { phenotypeId: string }) {
  const pheno = useProfilesStore((s) => s.phenotypesById[phenotypeId]);
  const profile = useProfilesStore((s) =>
    s.profiles.find((p) => p.id === phenotypeId),
  );
  const activeId = useProfilesStore((s) => s.activeId);
  const saving = useProfilesStore((s) => s.saving);
  const error = useProfilesStore((s) => s.error);
  const setActive = useProfilesStore((s) => s.setActive);
  const savePhenotype = useProfilesStore((s) => s.savePhenotype);
  const duplicatePhenotype = useProfilesStore((s) => s.duplicatePhenotype);

  const registry = useModelConfigStore((s) => s.registry);
  const registryLoading = useModelConfigStore((s) => s.loading);
  const modelsById = useModelConfigStore((s) => s.modelsById);
  const loadModels = useModelConfigStore((s) => s.loadModels);
  const loadRegistry = useModelConfigStore((s) => s.load);

  // Hydrate the registry once (the Model section also loads it, but the editor must
  // not depend on that pane being opened first). The shared store dedupes the fetch.
  useEffect(() => {
    if (!registry && !registryLoading) void loadRegistry();
  }, [registry, registryLoading, loadRegistry]);

  if (!pheno) return null;

  const isDefault = phenotypeId === DEFAULT_PROFILE_ID;
  const isActive = phenotypeId === activeId;
  const connections = registry?.connections ?? [];
  const providerConn = connections.find((c) => c.id === pheno.provider);
  // The connection whose models populate the Model row: the bound provider, else the
  // global active connection (what an un-bound phenotype inherits).
  const modelConnId = pheno.provider ?? registry?.active;
  const models = modelConnId ? modelsById[modelConnId] : undefined;

  const loadModelsFor = (id: string | undefined) => {
    if (id && modelsById[id] === undefined) void loadModels(id);
  };

  return (
    <section className="space-y-3 border-t pt-5" aria-label="Phenotype editor">
      <header className="flex items-center gap-2">
        <span className="min-w-0 truncate text-[13px] font-medium text-foreground">
          {profile?.name ?? pheno.name}
        </span>
        {isDefault ? (
          <Lock
            className="size-3 shrink-0 text-muted-foreground"
            aria-label="Built-in default (immutable)"
          />
        ) : null}
        {isActive ? (
          <span className="rounded-full bg-primary/10 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-primary">
            Active
          </span>
        ) : (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="ml-auto h-6"
            disabled={saving}
            onClick={() => void setActive(phenotypeId)}
          >
            Activate
          </Button>
        )}
      </header>

      {isDefault ? (
        // The built-in default is immutable — never write it. Offer a functional
        // clone the user can customize instead.
        <div className="space-y-2.5">
          <ReadOnlyRow
            icon={<Server />}
            label="Provider"
            value="Global default"
          />
          <ReadOnlyRow icon={<Cpu />} label="Model" value="Global default" />
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            The built-in default is read-only. Duplicate it to bind a provider
            and model.
          </p>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={saving}
            onClick={() => void duplicatePhenotype(phenotypeId)}
          >
            <Copy />
            Duplicate to customize
          </Button>
        </div>
      ) : connections.length === 0 ? (
        <p className="text-[12px] text-muted-foreground">
          {registryLoading
            ? "Loading connections…"
            : "No provider connections — add one in the Model section to bind a provider."}
        </p>
      ) : (
        <div className="space-y-2.5">
          {/* Provider row — pick a connection or inherit the global default. */}
          <EditorRow icon={<Server />} label="Provider">
            <PickerButton
              disabled={saving}
              valued={Boolean(pheno.provider)}
              label={
                pheno.provider
                  ? (providerConn?.displayName ?? pheno.provider)
                  : "Inherit (global default)"
              }
            >
              {connections.map((c) => (
                <DropdownMenuItem
                  key={c.id}
                  onSelect={() =>
                    void savePhenotype(phenotypeId, { provider: c.id })
                  }
                >
                  <Check
                    className={cn(
                      c.id === pheno.provider ? "opacity-100" : "opacity-0",
                    )}
                  />
                  <span className="min-w-0 truncate">{c.displayName}</span>
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuItem
                disabled={!pheno.provider}
                onSelect={() =>
                  void savePhenotype(phenotypeId, { provider: undefined })
                }
              >
                <span className="text-muted-foreground">
                  Inherit (global default)
                </span>
              </DropdownMenuItem>
            </PickerButton>
          </EditorRow>

          {/* Model row — pick from the (bound or global) connection's models. */}
          <EditorRow icon={<Cpu />} label="Model">
            <PickerButton
              disabled={saving}
              valued={Boolean(pheno.model)}
              onOpen={() => loadModelsFor(modelConnId)}
              label={pheno.model ?? "Inherit (connection default)"}
            >
              {models === undefined ? (
                <DropdownMenuItem disabled>Loading…</DropdownMenuItem>
              ) : models.length === 0 ? (
                <DropdownMenuItem disabled>No models</DropdownMenuItem>
              ) : (
                models.map((m) => (
                  <DropdownMenuItem
                    key={m}
                    onSelect={() =>
                      void savePhenotype(phenotypeId, { model: m })
                    }
                  >
                    <Check
                      className={cn(
                        m === pheno.model ? "opacity-100" : "opacity-0",
                      )}
                    />
                    <span className="min-w-0 truncate">{m}</span>
                  </DropdownMenuItem>
                ))
              )}
              <DropdownMenuSeparator />
              <DropdownMenuItem
                disabled={!pheno.model}
                onSelect={() =>
                  void savePhenotype(phenotypeId, { model: undefined })
                }
              >
                <span className="text-muted-foreground">
                  Inherit (connection default)
                </span>
              </DropdownMenuItem>
            </PickerButton>
          </EditorRow>
        </div>
      )}

      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}
    </section>
  );
}

function EditorRow({
  icon,
  label,
  children,
}: {
  icon: ReactNode;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center gap-3">
      <span className="flex w-20 shrink-0 items-center gap-1.5 text-[12px] text-muted-foreground">
        <span className="[&>svg]:size-3.5">{icon}</span>
        {label}
      </span>
      {children}
    </div>
  );
}

function ReadOnlyRow({
  icon,
  label,
  value,
}: {
  icon: ReactNode;
  label: string;
  value: string;
}) {
  return (
    <EditorRow icon={icon} label={label}>
      <span className="text-[12px] text-muted-foreground/80">{value}</span>
    </EditorRow>
  );
}

// A select-like trigger that opens a menu of choices. Mirrors the model-chip picker
// surface but as a single-level menu per row.
function PickerButton({
  label,
  valued,
  disabled,
  onOpen,
  children,
}: {
  label: string;
  valued: boolean;
  disabled?: boolean;
  onOpen?: () => void;
  children: ReactNode;
}) {
  return (
    <DropdownMenu onOpenChange={(open) => open && onOpen?.()}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          disabled={disabled}
          className="h-7 min-w-0 max-w-[60%] flex-1 justify-between gap-1.5 font-normal"
        >
          <span
            className={cn(
              "min-w-0 truncate",
              valued ? "text-foreground" : "text-muted-foreground",
            )}
          >
            {label}
          </span>
          <ChevronDown className="size-3 shrink-0 opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="max-h-72 min-w-48 max-w-64 overflow-y-auto"
      >
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
