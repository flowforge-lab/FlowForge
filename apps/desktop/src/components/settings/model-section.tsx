import { useEffect } from "react";
import { cn } from "@/lib/utils";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { SettingsSwitch } from "@/components/settings/switch";
import { SettingsSlider } from "@/components/settings/slider";
import { useSettingsStore } from "@/store/settings";
import {
  SUMMARY_THRESHOLD_MAX,
  SUMMARY_THRESHOLD_MIN,
  useModelConfigStore,
  type Effort,
} from "@/store/model-config";
import type { ProviderKind } from "@/bindings/ProviderKind";

const PROVIDER_OPTIONS: ReadonlyArray<{ value: ProviderKind; label: string }> =
  [
    { value: "candleVllm", label: "candle-vLLM" },
    { value: "ollama", label: "Ollama" },
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
 * Model section (#126): provider + model picker (durable, via IPC) plus reasoning
 * controls (thinking / effort / summarization threshold, persisted locally).
 * Registers `resetModel` for the footer "Reset to defaults".
 */
export function ModelSection() {
  const provider = useModelConfigStore((s) => s.provider);
  const models = useModelConfigStore((s) => s.models);
  const loading = useModelConfigStore((s) => s.loading);
  const saving = useModelConfigStore((s) => s.saving);
  const error = useModelConfigStore((s) => s.error);
  const load = useModelConfigStore((s) => s.load);
  const setKind = useModelConfigStore((s) => s.setKind);
  const setModel = useModelConfigStore((s) => s.setModel);

  const thinking = useModelConfigStore((s) => s.thinking);
  const effort = useModelConfigStore((s) => s.effort);
  const summarizationThreshold = useModelConfigStore(
    (s) => s.summarizationThreshold,
  );
  const setThinking = useModelConfigStore((s) => s.setThinking);
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

  // If the persisted model isn't one the active provider offers (e.g. right
  // after a kind switch), fall back to the first available so the dropdown
  // never selects a model the provider can't serve.
  useEffect(() => {
    if (provider && models.length > 0 && !models.includes(provider.model)) {
      void setModel(models[0]);
    }
  }, [provider, models, setModel]);

  // Always include the current model so the select can show it even if it's not
  // in the (best-effort) list yet.
  const modelOptions =
    provider && !models.includes(provider.model)
      ? [provider.model, ...models]
      : models;

  return (
    <div className="space-y-6">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">Provider</h3>
        {error ? (
          <p className="text-[12px] text-destructive" role="alert">
            {error}
          </p>
        ) : null}
        {loading && !provider ? (
          <p className="text-[12px] text-muted-foreground">Loading…</p>
        ) : provider ? (
          <SegmentedControl
            label="Provider"
            options={PROVIDER_OPTIONS}
            value={provider.kind}
            onValueChange={(kind) => void setKind(kind)}
            disabled={saving}
          />
        ) : null}
      </section>

      {provider ? (
        <section className="space-y-1.5">
          <label
            htmlFor="model-select"
            className="text-[13px] font-medium text-foreground"
          >
            Chat model
          </label>
          <select
            id="model-select"
            value={provider.model}
            disabled={saving || loading || modelOptions.length === 0}
            onChange={(e) => void setModel(e.target.value)}
            className={cn(
              "h-8 w-full rounded-lg border border-input bg-transparent px-2.5 text-[13px] text-foreground outline-none transition-colors",
              "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
              "disabled:cursor-not-allowed disabled:opacity-50 dark:bg-input/30",
            )}
          >
            {modelOptions.map((model) => (
              <option key={model} value={model}>
                {model}
              </option>
            ))}
          </select>
          {!loading && provider && models.length === 0 ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              No models installed for this provider. Pull one (e.g.{" "}
              <code className="rounded bg-muted px-1 py-0.5">
                ollama pull {provider.model}
              </code>
              ), then reopen settings.
            </p>
          ) : (
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              Sent on each chat request. API keys and hosted providers arrive
              later (#8).
            </p>
          )}
        </section>
      ) : null}

      <section className="space-y-5 border-t pt-5">
        <SettingsSwitch
          label="Thinking"
          description="Let the model reason before answering."
          checked={thinking}
          onCheckedChange={setThinking}
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
