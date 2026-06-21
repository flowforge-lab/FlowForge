import { useEffect } from "react";
import { Plus, Info } from "lucide-react";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { SettingsSwitch } from "@/components/settings/switch";
import { SettingsSlider } from "@/components/settings/slider";
import { ProviderCard } from "@/components/settings/provider-card";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { useSettingsStore } from "@/store/settings";
import {
  SUMMARY_THRESHOLD_MAX,
  SUMMARY_THRESHOLD_MIN,
  activeConnection,
  useModelConfigStore,
  type Effort,
} from "@/store/model-config";
import type { ProviderKind } from "@/bindings/ProviderKind";

/** Kinds the backend can serve (matches `ProviderKind`). Used to offer only
 *  add-able providers — the registry seeds candle-vLLM + Ollama, so in practice
 *  this surfaces AWS Bedrock. */
const ADDABLE: ReadonlyArray<{ kind: ProviderKind; label: string }> = [
  { kind: "candleVllm", label: "candle-vLLM" },
  { kind: "ollama", label: "Ollama" },
  { kind: "bedrock", label: "AWS Bedrock" },
];

const EFFORT_OPTIONS: ReadonlyArray<{ value: Effort; label: string }> = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
];

/** Tokens → compact "150k" readout. */
function formatThreshold(tokens: number): string {
  return `${Math.round(tokens / 1000)}k`;
}

/**
 * Model section (#126, #202 PR-3b): a provider-centric accordion. Each registry
 * connection is a card carrying its own credentials + default-model picker (durable
 * via IPC; secrets write-only to the OS keychain). Global reasoning controls
 * (thinking via IPC on the active connection; effort / summarization threshold
 * persisted locally) sit below. Registers `resetModel` for the footer reset.
 */
export function ModelSection() {
  const registry = useModelConfigStore((s) => s.registry);
  const loading = useModelConfigStore((s) => s.loading);
  const saving = useModelConfigStore((s) => s.saving);
  const error = useModelConfigStore((s) => s.error);
  const load = useModelConfigStore((s) => s.load);
  const addConnection = useModelConfigStore((s) => s.addConnection);
  const setThinking = useModelConfigStore((s) => s.setThinking);

  const effort = useModelConfigStore((s) => s.effort);
  const summarizationThreshold = useModelConfigStore(
    (s) => s.summarizationThreshold,
  );
  const setEffort = useModelConfigStore((s) => s.setEffort);
  const setSummarizationThreshold = useModelConfigStore(
    (s) => s.setSummarizationThreshold,
  );
  const resetModel = useModelConfigStore((s) => s.resetModel);

  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    registerResetHandler(resetModel);
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetModel]);

  const active = activeConnection(registry);
  const thinking = active?.thinking ?? true;
  const present = new Set(registry?.connections.map((c) => c.kind) ?? []);
  const addable = ADDABLE.filter((a) => !present.has(a.kind));

  return (
    <div className="space-y-6">
      <section className="space-y-3">
        <h3 className="text-[13px] font-medium text-foreground">Providers</h3>

        <div className="flex items-start gap-2 rounded-lg border border-sky-500/20 bg-sky-500/5 px-3 py-2 text-[12px] leading-relaxed text-sky-700 dark:text-sky-300">
          <Info className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          <span>
            The <strong>default model</strong> is used when no session or
            phenotype specifies one. Credentials and models live together under
            each provider — secrets are stored in your OS keychain, never on
            disk or in this app's config.
          </span>
        </div>

        {error ? (
          <p className="text-[12px] text-destructive" role="alert">
            {error}
          </p>
        ) : null}

        {loading && !registry ? (
          <p className="text-[12px] text-muted-foreground">Loading…</p>
        ) : registry ? (
          <div className="space-y-2.5">
            {registry.connections.map((conn) => (
              <ProviderCard key={conn.id} conn={conn} />
            ))}

            {addable.length > 0 ? (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    disabled={saving}
                    className="flex w-full items-center gap-2 rounded-xl border border-dashed border-border px-4 py-3 text-[13px] font-medium text-primary transition-colors hover:bg-primary/5 disabled:opacity-50"
                  >
                    <Plus className="size-4" /> Add provider
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" className="w-56">
                  {addable.map((a) => (
                    <DropdownMenuItem
                      key={a.kind}
                      onSelect={() => void addConnection(a.kind)}
                    >
                      {a.label}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            ) : null}
          </div>
        ) : null}
      </section>

      <section className="space-y-5 border-t pt-5">
        <SettingsSwitch
          label="Thinking"
          description="Let the default model reason before answering."
          checked={thinking}
          disabled={!active || saving}
          onCheckedChange={(v) => {
            if (active) void setThinking(active.id, v);
          }}
        />

        <div className="space-y-2">
          <h3 className="text-[13px] font-medium text-foreground">Effort</h3>
          <SegmentedControl
            label="Reasoning effort"
            options={EFFORT_OPTIONS}
            value={effort}
            onValueChange={setEffort}
            disabled={!thinking}
          />
        </div>

        <SettingsSlider
          label="Summarization threshold"
          value={summarizationThreshold}
          onValueChange={setSummarizationThreshold}
          min={SUMMARY_THRESHOLD_MIN}
          max={SUMMARY_THRESHOLD_MAX}
          step={10_000}
          formatValue={formatThreshold}
        />
        <p className="-mt-3 text-[11px] leading-relaxed text-muted-foreground">
          Context size at which older turns are summarized.
        </p>
      </section>
    </div>
  );
}
