import { useEffect } from "react";
import { Plus, Info } from "@/components/ui/icon";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useSettingsStore } from "@/store/settings";
import {
  NUM_CTX_PRESETS,
  SUMMARY_THRESHOLD_MAX,
  SUMMARY_THRESHOLD_MIN,
  activeConnection,
  reasoningToggleNoOp,
  isLocalKind,
  useModelConfigStore,
  type Effort,
} from "@/store/model-config";
import type { ProviderKind } from "@/bindings/ProviderKind";

/** Kinds the backend can serve (matches `ProviderKind`). Used to offer only
 *  add-able providers — the registry seeds candle-vLLM + Ollama, so in practice
 *  this surfaces AWS Bedrock and the hosted OpenAI-compatible providers. */
const ADDABLE: ReadonlyArray<{ kind: ProviderKind; label: string }> = [
  { kind: "candleVllm", label: "candle-vLLM" },
  { kind: "ollama", label: "Ollama" },
  { kind: "bedrock", label: "AWS Bedrock" },
  { kind: "openai", label: "OpenAI" },
  { kind: "siliconFlow", label: "SiliconFlow" },
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

/** Sentinel `Select` value for "Auto" (unset num_ctx); radix needs a value. */
const NUM_CTX_AUTO = "auto";

/** Tokens → compact "8K" / "128K" label for the context-window presets. */
function formatNumCtx(tokens: number): string {
  return `${tokens / 1024}K`;
}

/**
 * Model section (#126, #202 PR-3b): a provider-centric accordion. Each registry
 * connection is a card carrying its own credentials + default-model picker (durable
 * via IPC; secrets write-only to the OS keychain). Reasoning controls for the active
 * connection (thinking + effort, both per-connection via IPC) and the locally-persisted
 * summarization threshold sit below. Registers `resetModel` for the footer reset.
 */
export function ModelSection() {
  const registry = useModelConfigStore((s) => s.registry);
  const loading = useModelConfigStore((s) => s.loading);
  const saving = useModelConfigStore((s) => s.saving);
  const error = useModelConfigStore((s) => s.error);
  const load = useModelConfigStore((s) => s.load);
  const addConnection = useModelConfigStore((s) => s.addConnection);
  const setThinking = useModelConfigStore((s) => s.setThinking);
  const setWarmupEnabled = useModelConfigStore((s) => s.setWarmupEnabled);
  const setNumCtx = useModelConfigStore((s) => s.setNumCtx);

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
  // Effort is now a per-connection backend field (#395); read the active one.
  const effort = active?.reasoningEffort ?? "medium";
  // For OpenAI / SiliconFlow the backend sends no reasoning-*enable* param, so the
  // Thinking toggle has no wire effect (reasoning still streams when the model
  // emits it). Disable it with an honest hint rather than letting it read as a
  // working on/off switch (#311).
  const reasoningToggleNoEffect = active
    ? reasoningToggleNoOp(active.kind)
    : false;
  // Warmup only helps local backends (#61); hide the control for hosted kinds.
  const warmupIsLocal = active ? isLocalKind(active.kind) : false;
  const warmupEnabled = active?.warmupEnabled ?? true;
  // num_ctx only reaches the wire on the Ollama provider (#651); candle-vLLM — the
  // other local kind — serves its window at launch, so the control would be a no-op
  // there. Gate to Ollama only rather than all local kinds (#311: no dead controls).
  const numCtxIsOllama = active?.kind === "ollama";
  const numCtx = active?.numCtx;
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
          description={
            reasoningToggleNoEffect
              ? "This provider doesn't support toggling reasoning; it's shown when the model emits it."
              : "Let the default model reason before answering."
          }
          checked={thinking}
          disabled={!active || saving || reasoningToggleNoEffect}
          onCheckedChange={(v) => {
            if (active) void setThinking(active.id, v);
          }}
        />

        {warmupIsLocal ? (
          <SettingsSwitch
            label="Warmup"
            description="Nudge the local model while you type so the first token streams instantly. Turn off to avoid sustained GPU use (e.g. on laptop battery)."
            checked={warmupEnabled}
            disabled={!active || saving}
            onCheckedChange={(v) => {
              if (active) void setWarmupEnabled(active.id, v);
            }}
          />
        ) : null}

        {numCtxIsOllama ? (
          <div className="flex items-center justify-between gap-4">
            <span className="flex flex-col gap-0.5">
              <span className="text-[13px] font-medium text-foreground">
                Context window
              </span>
              <span className="text-[12px] leading-relaxed text-muted-foreground">
                Tokens Ollama is asked to serve. Auto uses the server default;
                larger widens context but uses more memory.
              </span>
            </span>
            <Select
              value={numCtx?.toString() ?? NUM_CTX_AUTO}
              onValueChange={(v) => {
                if (active)
                  void setNumCtx(
                    active.id,
                    v === NUM_CTX_AUTO ? null : Number(v),
                  );
              }}
              disabled={!active || saving}
            >
              <SelectTrigger className="w-28" aria-label="Context window">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NUM_CTX_AUTO}>Auto</SelectItem>
                {NUM_CTX_PRESETS.map((tokens) => (
                  <SelectItem key={tokens} value={tokens.toString()}>
                    {formatNumCtx(tokens)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        ) : null}

        <div className="space-y-2">
          <h3 className="text-[13px] font-medium text-foreground">Effort</h3>
          <SegmentedControl
            label="Reasoning effort"
            options={EFFORT_OPTIONS}
            value={effort}
            onValueChange={(v) => {
              if (active) void setEffort(active.id, v);
            }}
            disabled={!active || saving || !thinking}
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
