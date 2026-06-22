import { useState } from "react";
import { ChevronDown, Plus, Loader2, Check, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { SegmentedControl } from "@/components/settings/segmented-control";
import {
  isHostedKeyKind,
  normalizeBaseUrl,
  useModelConfigStore,
  type TestState,
} from "@/store/model-config";
import type { ProviderConnection } from "@/bindings/ProviderConnection";
import type { BedrockAuth } from "@/bindings/BedrockAuth";

/** Per-kind presentation (badge glyph/color, tier label). */
const KIND_META: Record<
  ProviderConnection["kind"],
  { badge: string; color: string; tier: "Local" | "Hosted" }
> = {
  candleVllm: { badge: "C", color: "#4a9eff", tier: "Local" },
  ollama: { badge: "O", color: "#8bc34a", tier: "Local" },
  bedrock: { badge: "B", color: "#ff9900", tier: "Hosted" },
  openai: { badge: "AI", color: "#10a37f", tier: "Hosted" },
  siliconFlow: { badge: "SF", color: "#6d28d9", tier: "Hosted" },
};

const AUTH_OPTIONS: ReadonlyArray<{ value: BedrockAuth; label: string }> = [
  { value: "profile", label: "Profile" },
  { value: "iamKeys", label: "IAM Keys" },
  { value: "apiKey", label: "API Key" },
];

const REGION_SUGGESTIONS = [
  "us-east-1",
  "us-west-2",
  "eu-central-1",
  "eu-west-1",
  "ap-northeast-1",
  "ap-southeast-1",
];

function authLabel(mode: BedrockAuth | undefined): string {
  switch (mode) {
    case "iamKeys":
      return "IAM keys";
    case "apiKey":
      return "API key";
    default:
      return "profile";
  }
}

/** Base-URL field copy per hosted kind: the default endpoint (shown as the
 *  placeholder) plus a hint. SiliconFlow's hint points `.cn` users at the China
 *  endpoint; OpenAI's notes the optional compatible-gateway override. The field is
 *  editable so users can switch regions/gateways without an enum. */
function hostedBaseUrlMeta(kind: ProviderConnection["kind"]): {
  placeholder: string;
  hint: string;
} {
  if (kind === "siliconFlow") {
    return {
      placeholder: "https://api.siliconflow.com/v1",
      hint: "Default is the global endpoint. Use https://api.siliconflow.cn/v1 for the China region.",
    };
  }
  return {
    placeholder: "https://api.openai.com/v1",
    hint: "Leave blank for the default OpenAI endpoint, or set an OpenAI-compatible base URL (e.g. OpenRouter, Azure).",
  };
}

function fieldLabelClass() {
  return "text-[11px] font-semibold tracking-wide text-muted-foreground uppercase";
}

/** A single expandable provider connection: credentials + default-model picker.
 *  Bedrock gets the region / auth-mode / write-only secret form; local kinds get
 *  a base-URL field. All writes go through the model-config store (registry +
 *  write-only secret commands); the secret value is never read back. */
export function ProviderCard({ conn }: { conn: ProviderConnection }) {
  const isDefault = useModelConfigStore((s) => s.registry?.active === conn.id);
  const connectionCount = useModelConfigStore(
    (s) => s.registry?.connections.length ?? 0,
  );
  const models = useModelConfigStore((s) => s.modelsById[conn.id]);
  const test = useModelConfigStore((s) => s.test[conn.id]);
  const saving = useModelConfigStore((s) => s.saving);
  const loadModels = useModelConfigStore((s) => s.loadModels);
  const setDefaultModel = useModelConfigStore((s) => s.setDefaultModel);
  const saveConnection = useModelConfigStore((s) => s.saveConnection);
  const removeConnection = useModelConfigStore((s) => s.removeConnection);
  const setSecret = useModelConfigStore((s) => s.setSecret);
  const clearSecret = useModelConfigStore((s) => s.clearSecret);
  const testConnection = useModelConfigStore((s) => s.testConnection);
  const clearTest = useModelConfigStore((s) => s.clearTest);

  const [open, setOpen] = useState(isDefault);
  const meta = KIND_META[conn.kind];
  const isBedrock = conn.kind === "bedrock";
  const isHostedKey = isHostedKeyKind(conn.kind);

  // Editable non-secret fields, seeded from the stored connection.
  const [baseUrl, setBaseUrl] = useState(conn.baseUrl ?? "");
  const [region, setRegion] = useState(conn.region ?? "us-east-1");
  const [authMode, setAuthMode] = useState<BedrockAuth>(
    conn.authMode ?? "profile",
  );
  const [awsProfile, setAwsProfile] = useState(conn.awsProfile ?? "");
  const [accessKeyId, setAccessKeyId] = useState(conn.accessKeyId ?? "");
  // Write-only secret inputs — never seeded from the backend, cleared on save.
  const [secretAccessKey, setSecretAccessKey] = useState("");
  const [sessionToken, setSessionToken] = useState("");
  const [apiKey, setApiKey] = useState("");

  // Inline "Add model" discovery flow.
  const [discovering, setDiscovering] = useState(false);
  const [discoverOpen, setDiscoverOpen] = useState(false);

  const configured = isBedrock
    ? authMode === "profile" || conn.hasKey
    : isHostedKey
      ? conn.hasKey
      : true;
  const detail = isBedrock
    ? `${conn.region ?? region} · ${authLabel(conn.authMode)}`
    : isHostedKey
      ? (conn.baseUrl ?? hostedBaseUrlMeta(conn.kind).placeholder)
      : (conn.baseUrl ?? "default endpoint");

  // Radio rows show only the persisted default; "Add model" discovery lists the
  // rest of the fetched catalog (everything that isn't already the default).
  const shown = conn.model ? [conn.model] : [];
  const undiscovered = (models ?? []).filter((m) => m !== conn.model);

  const expand = () => {
    const next = !open;
    setOpen(next);
    if (next && models === undefined) void loadModels(conn.id);
  };

  const onDiscover = async () => {
    setDiscoverOpen(true);
    setDiscovering(true);
    await loadModels(conn.id);
    setDiscovering(false);
  };

  const onSave = async () => {
    clearTest(conn.id);
    if (isBedrock) {
      await saveConnection({
        ...conn,
        region: region.trim() || undefined,
        authMode,
        awsProfile: awsProfile.trim() || undefined,
        accessKeyId: accessKeyId.trim() || undefined,
      });
      // Persist any freshly-entered secrets (write-only), then drop them locally.
      if (authMode === "iamKeys") {
        if (secretAccessKey)
          await setSecret(conn.id, "secretAccessKey", secretAccessKey);
        if (sessionToken)
          await setSecret(conn.id, "sessionToken", sessionToken);
      } else if (authMode === "apiKey") {
        if (apiKey) await setSecret(conn.id, "apiKey", apiKey);
      }
      setSecretAccessKey("");
      setSessionToken("");
      setApiKey("");
    } else if (isHostedKey) {
      // Canonicalize so a pasted `…/v1/chat/completions` doesn't become
      // `…/chat/completions/models` on the model/probe call.
      const normalized = normalizeBaseUrl(baseUrl);
      setBaseUrl(normalized);
      await saveConnection({ ...conn, baseUrl: normalized || undefined });
      // Persist a freshly-entered key (write-only), then drop it locally.
      if (apiKey) await setSecret(conn.id, "apiKey", apiKey);
      setApiKey("");
    } else {
      const normalized = normalizeBaseUrl(baseUrl);
      setBaseUrl(normalized);
      await saveConnection({ ...conn, baseUrl: normalized || undefined });
    }
  };

  return (
    <div
      className={cn(
        "overflow-hidden rounded-xl border bg-card transition-colors",
        open ? "border-primary/40" : "border-border",
      )}
    >
      <button
        type="button"
        onClick={expand}
        aria-expanded={open}
        className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-muted/40"
      >
        <span
          className="flex size-8 shrink-0 items-center justify-center rounded-full text-[12px] font-bold text-white"
          style={{ background: meta.color }}
          aria-hidden
        >
          {meta.badge}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="truncate text-[13px] font-semibold text-foreground">
              {conn.displayName}
            </span>
            <span
              className={cn(
                "rounded-full px-1.5 py-0.5 text-[9px] font-semibold tracking-wide uppercase",
                configured
                  ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                  : "bg-muted text-muted-foreground",
              )}
            >
              {configured ? "Configured" : "Not configured"}
            </span>
          </span>
          <span className="mt-0.5 block truncate font-mono text-[11px] text-muted-foreground">
            {detail}
          </span>
        </span>
        <span className="shrink-0 text-[10px] tracking-wide text-muted-foreground/70 uppercase">
          {meta.tier}
        </span>
        <ChevronDown
          className={cn(
            "size-4 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-180",
          )}
          aria-hidden
        />
      </button>

      {open ? (
        <div className="border-t border-border">
          {/* ── Credentials ─────────────────────────────────── */}
          <section className="space-y-3.5 border-b border-border px-4 py-4">
            <h4 className={fieldLabelClass()}>Credentials</h4>

            {isBedrock ? (
              <>
                <div className="space-y-1.5">
                  <label
                    htmlFor={`region-${conn.id}`}
                    className={fieldLabelClass()}
                  >
                    Region
                  </label>
                  <Input
                    id={`region-${conn.id}`}
                    list={`regions-${conn.id}`}
                    value={region}
                    placeholder="us-east-1"
                    onChange={(e) => setRegion(e.target.value)}
                  />
                  <datalist id={`regions-${conn.id}`}>
                    {REGION_SUGGESTIONS.map((r) => (
                      <option key={r} value={r} />
                    ))}
                  </datalist>
                  <p className="text-[11px] text-muted-foreground">
                    bedrock-runtime.&lt;region&gt;.amazonaws.com
                  </p>
                </div>

                <div className="space-y-1.5">
                  <span className={fieldLabelClass()}>Authentication</span>
                  <SegmentedControl
                    label="Bedrock authentication mode"
                    options={AUTH_OPTIONS}
                    value={authMode}
                    onValueChange={setAuthMode}
                  />
                </div>

                {authMode === "profile" ? (
                  <div className="space-y-1.5">
                    <label
                      htmlFor={`profile-${conn.id}`}
                      className={fieldLabelClass()}
                    >
                      AWS Profile
                    </label>
                    <Input
                      id={`profile-${conn.id}`}
                      value={awsProfile}
                      placeholder="default"
                      onChange={(e) => setAwsProfile(e.target.value)}
                    />
                    <p className="text-[11px] text-muted-foreground">
                      Reads from ~/.aws/config. Leave blank for the default
                      profile.
                    </p>
                  </div>
                ) : null}

                {authMode === "iamKeys" ? (
                  <>
                    <div className="space-y-1.5">
                      <label
                        htmlFor={`akid-${conn.id}`}
                        className={fieldLabelClass()}
                      >
                        Access Key ID
                      </label>
                      <Input
                        id={`akid-${conn.id}`}
                        value={accessKeyId}
                        placeholder="AKIA…"
                        onChange={(e) => setAccessKeyId(e.target.value)}
                      />
                    </div>
                    <SecretField
                      id={`sak-${conn.id}`}
                      label="Secret Access Key"
                      placeholder="••••••"
                      value={secretAccessKey}
                      onChange={setSecretAccessKey}
                      hasKey={conn.hasKey}
                      onClear={() =>
                        void clearSecret(conn.id, "secretAccessKey")
                      }
                    />
                    <SecretField
                      id={`stk-${conn.id}`}
                      label="Session Token (optional)"
                      placeholder="For temporary credentials"
                      value={sessionToken}
                      onChange={setSessionToken}
                      hasKey={conn.hasKey}
                      hint="Stored in the OS keychain. Never written to disk."
                      onClear={() => void clearSecret(conn.id, "sessionToken")}
                    />
                  </>
                ) : null}

                {authMode === "apiKey" ? (
                  <SecretField
                    id={`brk-${conn.id}`}
                    label="Bedrock API Key"
                    placeholder="br-…"
                    value={apiKey}
                    onChange={setApiKey}
                    hasKey={conn.hasKey}
                    hint="Stored securely in the OS keychain."
                    onClear={() => void clearSecret(conn.id, "apiKey")}
                  />
                ) : null}
              </>
            ) : isHostedKey ? (
              <>
                <div className="space-y-1.5">
                  <label
                    htmlFor={`baseurl-${conn.id}`}
                    className={fieldLabelClass()}
                  >
                    Base URL
                  </label>
                  <Input
                    id={`baseurl-${conn.id}`}
                    value={baseUrl}
                    placeholder={hostedBaseUrlMeta(conn.kind).placeholder}
                    onChange={(e) => setBaseUrl(e.target.value)}
                  />
                  <p className="text-[11px] text-muted-foreground">
                    {hostedBaseUrlMeta(conn.kind).hint}
                  </p>
                </div>
                <SecretField
                  id={`apikey-${conn.id}`}
                  label="API Key"
                  placeholder="sk-…"
                  value={apiKey}
                  onChange={setApiKey}
                  hasKey={conn.hasKey}
                  hint="Stored securely in the OS keychain."
                  onClear={() => void clearSecret(conn.id, "apiKey")}
                />
              </>
            ) : (
              <div className="space-y-1.5">
                <label
                  htmlFor={`baseurl-${conn.id}`}
                  className={fieldLabelClass()}
                >
                  Host (optional)
                </label>
                <Input
                  id={`baseurl-${conn.id}`}
                  value={baseUrl}
                  placeholder={
                    conn.kind === "ollama"
                      ? "http://localhost:11434"
                      : "http://localhost:8000/v1"
                  }
                  onChange={(e) => setBaseUrl(e.target.value)}
                />
                <p className="text-[11px] text-muted-foreground">
                  Leave blank for the default local endpoint.
                </p>
              </div>
            )}

            <div className="flex items-center gap-2 pt-0.5">
              <TestResult test={test} />
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="ml-auto"
                disabled={test?.state === "loading"}
                onClick={() => void testConnection(conn.id)}
              >
                {test?.state === "loading" ? (
                  <Loader2 className="animate-spin" />
                ) : null}
                Test Connection
              </Button>
              <Button
                type="button"
                size="sm"
                disabled={saving}
                onClick={() => void onSave()}
              >
                Save
              </Button>
            </div>
          </section>

          {/* ── Models ──────────────────────────────────────── */}
          <section className="px-4 py-3">
            <h4 className={cn(fieldLabelClass(), "mb-1")}>Models</h4>
            {shown.length === 0 ? (
              <p className="px-1 py-2 text-[12px] text-muted-foreground">
                No default model yet. Add one below.
              </p>
            ) : (
              <ul className="-mx-1 max-h-80 overflow-y-auto">
                {shown.map((model) => {
                  const active = isDefault && conn.model === model;
                  return (
                    <li key={model}>
                      <button
                        type="button"
                        onClick={() => void setDefaultModel(conn.id, model)}
                        className={cn(
                          "flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left transition-colors hover:bg-muted/40",
                          active && "bg-primary/5",
                        )}
                      >
                        <span
                          className={cn(
                            "flex size-4 shrink-0 items-center justify-center rounded-full border-2",
                            active
                              ? "border-primary"
                              : "border-muted-foreground/50",
                          )}
                          aria-hidden
                        >
                          {active ? (
                            <span className="size-2 rounded-full bg-primary" />
                          ) : null}
                        </span>
                        <span className="flex-1 truncate font-mono text-[12px] text-foreground">
                          {model}
                        </span>
                        {active ? (
                          <span className="rounded-md border border-primary/30 bg-primary/5 px-1.5 py-0.5 text-[9px] font-semibold tracking-wide text-primary uppercase">
                            Default
                          </span>
                        ) : null}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}

            {discoverOpen ? (
              <div className="mt-2 rounded-lg border border-border bg-muted/30 p-3">
                <p className="mb-2 text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
                  {isBedrock
                    ? "Available inference profiles"
                    : "Available models"}
                </p>
                {discovering ? (
                  <p className="flex items-center gap-2 text-[12px] text-muted-foreground">
                    <Loader2 className="size-3.5 animate-spin" />
                    {isBedrock
                      ? "Fetching via ListInferenceProfiles…"
                      : "Fetching models…"}
                  </p>
                ) : undiscovered.length === 0 ? (
                  <p className="text-[12px] text-muted-foreground">
                    No additional models found.
                  </p>
                ) : (
                  <ul className="-mx-1 max-h-64 overflow-y-auto">
                    {undiscovered.map((model) => (
                      <li key={model}>
                        <button
                          type="button"
                          onClick={() => {
                            void setDefaultModel(conn.id, model);
                            setDiscoverOpen(false);
                          }}
                          className="flex w-full items-center rounded-md px-2 py-1.5 text-left font-mono text-[12px] text-foreground transition-colors hover:bg-primary/5"
                        >
                          {model}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ) : (
              <button
                type="button"
                onClick={() => void onDiscover()}
                className="mt-1 flex items-center gap-1.5 rounded-lg px-3 py-2 text-[12px] font-medium text-primary transition-colors hover:bg-primary/5"
              >
                <Plus className="size-3.5" /> Add model
              </button>
            )}
          </section>

          {/* Remove (not for the last connection) */}
          {connectionCount > 1 ? (
            <div className="flex justify-end border-t border-border px-4 py-2.5">
              <Button
                type="button"
                variant="ghost"
                size="xs"
                className="text-muted-foreground hover:text-destructive"
                disabled={saving}
                onClick={() => void removeConnection(conn.id)}
              >
                <Trash2 className="size-3" /> Remove
              </Button>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** A write-only secret input with a keychain-stored indicator and clear action. */
function SecretField({
  id,
  label,
  placeholder,
  value,
  onChange,
  hasKey,
  hint,
  onClear,
}: {
  id: string;
  label: string;
  placeholder: string;
  value: string;
  onChange: (v: string) => void;
  hasKey?: boolean;
  hint?: string;
  onClear: () => void;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <label htmlFor={id} className={fieldLabelClass()}>
          {label}
        </label>
        {hasKey ? (
          <span className="flex items-center gap-1 text-[10px] font-medium text-emerald-600 dark:text-emerald-400">
            <Check className="size-3" /> Stored
            <button
              type="button"
              onClick={onClear}
              className="ml-1 text-muted-foreground underline-offset-2 hover:text-destructive hover:underline"
            >
              Clear
            </button>
          </span>
        ) : null}
      </div>
      <Input
        id={id}
        type="password"
        value={value}
        placeholder={hasKey ? "•••••• (stored)" : placeholder}
        autoComplete="off"
        onChange={(e) => onChange(e.target.value)}
      />
      {hint ? (
        <p className="text-[11px] text-muted-foreground">{hint}</p>
      ) : null}
    </div>
  );
}

/** Inline Test Connection outcome (ok / error); nothing while idle. */
function TestResult({ test }: { test: TestState | undefined }) {
  if (!test || test.state === "loading") return null;
  if (test.state === "ok") {
    return (
      <span className="flex items-center gap-1 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
        <Check className="size-3.5" /> Connected
      </span>
    );
  }
  return (
    <span className="text-[11px] text-destructive" role="alert">
      {test.message}
    </span>
  );
}
