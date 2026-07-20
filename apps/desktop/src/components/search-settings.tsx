import { useEffect } from "react";
import { cn } from "@/lib/utils";
import { keyStatusLabel, SEARCH_BACKENDS } from "@/lib/search";
import type { SearchBackend } from "@/bindings/SearchBackend";
import { Input } from "@/components/ui/input";
import { useSearchConfigStore } from "@/store/search-config";

function BackendOption({
  label,
  description,
  selected,
  keyLabel,
  onSelect,
}: {
  label: string;
  description: string;
  selected: boolean;
  keyLabel?: string;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onSelect}
      className={cn(
        "flex w-full flex-col gap-1 rounded-lg border px-3 py-2.5 text-left transition-colors",
        selected
          ? "border-primary bg-primary/5 ring-2 ring-primary/30"
          : "border-border bg-card hover:bg-muted/50",
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <span className="text-[12px] font-medium text-foreground">{label}</span>
        {keyLabel ? (
          <span className="shrink-0 text-[11px] text-muted-foreground">
            {keyLabel}
          </span>
        ) : null}
      </div>
      <span className="text-[11px] leading-relaxed text-muted-foreground">
        {description}
      </span>
    </button>
  );
}

/** Web-search backend picker — reads/writes `getSearchConfig` / `setSearchConfig`. */
export function SearchSettings() {
  const config = useSearchConfigStore((s) => s.config);
  const loading = useSearchConfigStore((s) => s.loading);
  const saving = useSearchConfigStore((s) => s.saving);
  const error = useSearchConfigStore((s) => s.error);
  const load = useSearchConfigStore((s) => s.load);
  const setBackend = useSearchConfigStore((s) => s.setBackend);
  const setBaseUrl = useSearchConfigStore((s) => s.setBaseUrl);
  const setEmail = useSearchConfigStore((s) => s.setEmail);

  useEffect(() => {
    void load();
  }, [load]);

  const activeMeta = config
    ? SEARCH_BACKENDS.find((b) => b.id === config.backend)
    : undefined;

  const onBackendSelect = (backend: SearchBackend) => {
    if (config?.backend === backend || saving) return;
    void setBackend(backend);
  };

  const onUrlBlur = (value: string) => {
    if (!config || config.backend !== "searxNg") return;
    const trimmed = value.trim();
    const current = config.baseUrl ?? "";
    if (trimmed === current) return;
    void setBaseUrl(trimmed);
  };

  const onEmailBlur = (value: string) => {
    if (!config) return;
    const trimmed = value.trim();
    const current = config.email ?? "";
    if (trimmed === current) return;
    void setEmail(trimmed);
  };

  return (
    <section className="mt-8 border-t pt-5">
      <h3 className="mb-1 text-[13px] font-medium text-foreground">
        Web search
      </h3>
      <p className="mb-3 text-[12px] leading-relaxed text-muted-foreground">
        Backend for the <code className="text-[11px]">web_search</code> tool.
        API keys are stored separately and never shown here.
      </p>

      {loading && !config ? (
        <p className="text-[12px] text-muted-foreground">Loading…</p>
      ) : null}

      {error ? (
        <p className="mb-3 text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}

      {config ? (
        <div className="space-y-3">
          <div className="flex flex-col gap-2">
            {SEARCH_BACKENDS.map((backend) => (
              <BackendOption
                key={backend.id}
                label={backend.label}
                description={backend.description}
                selected={config.backend === backend.id}
                keyLabel={
                  backend.requiresKey && config.backend === backend.id
                    ? keyStatusLabel(config.hasKey)
                    : undefined
                }
                onSelect={() => onBackendSelect(backend.id)}
              />
            ))}
          </div>

          {activeMeta?.showBaseUrl ? (
            <div className="space-y-1.5">
              <label
                htmlFor="search-base-url"
                className="text-[12px] font-medium text-foreground"
              >
                SearXNG base URL
              </label>
              <Input
                id="search-base-url"
                key={`searx-${config.baseUrl ?? ""}`}
                type="url"
                placeholder="https://searx.example.org"
                defaultValue={config.baseUrl ?? ""}
                disabled={saving}
                onBlur={(e) => onUrlBlur(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.currentTarget.blur();
                  }
                }}
              />
              <p className="text-[11px] leading-relaxed text-muted-foreground">
                Required for SearXNG. The tool appends{" "}
                <code className="text-[10px]">/search?format=json</code>.
              </p>
            </div>
          ) : null}

          <div className="space-y-1.5">
            <label
              htmlFor="search-email"
              className="text-[12px] font-medium text-foreground"
            >
              PubMed email
            </label>
            <Input
              id="search-email"
              key={`email-${config.email ?? ""}`}
              type="email"
              placeholder="you@example.com"
              defaultValue={config.email ?? ""}
              disabled={saving}
              onBlur={(e) => onEmailBlur(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.currentTarget.blur();
                }
              }}
            />
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              Optional. Sent to NCBI E-utilities for best-practice
              identification (#1021). They email you before blocking an IP for
              abuse.
            </p>
          </div>

          {activeMeta?.requiresKey ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              Hosted backends need an API key before{" "}
              <code className="text-[10px]">web_search</code> can run. Key entry
              arrives with provider settings (#8).
            </p>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
