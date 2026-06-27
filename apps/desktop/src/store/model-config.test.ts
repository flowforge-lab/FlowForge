// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  SUMMARY_THRESHOLD_MAX,
  SUMMARY_THRESHOLD_MIN,
  activeConnection,
  normalizeBaseUrl,
  useModelConfigStore,
} from "@/store/model-config";

describe("normalizeBaseUrl", () => {
  it("strips a trailing /chat/completions pasted from curl docs", () => {
    expect(
      normalizeBaseUrl("https://api.siliconflow.cn/v1/chat/completions"),
    ).toBe("https://api.siliconflow.cn/v1");
    expect(
      normalizeBaseUrl("https://api.siliconflow.cn/v1/chat/completions/"),
    ).toBe("https://api.siliconflow.cn/v1");
  });

  it("trims whitespace and trailing slashes", () => {
    expect(normalizeBaseUrl("  https://api.openai.com/v1/  ")).toBe(
      "https://api.openai.com/v1",
    );
  });

  it("leaves a correct base URL untouched and maps blank to empty", () => {
    expect(normalizeBaseUrl("https://api.openai.com/v1")).toBe(
      "https://api.openai.com/v1",
    );
    expect(normalizeBaseUrl("   ")).toBe("");
  });
});

describe("model-config reasoning controls", () => {
  beforeEach(() => {
    useModelConfigStore.getState().resetModel();
  });
  afterEach(() => {
    localStorage.clear();
  });

  it("defaults the 150k threshold", () => {
    expect(useModelConfigStore.getState().summarizationThreshold).toBe(150_000);
  });

  it("clamps the summarization threshold to its bounds", () => {
    useModelConfigStore.getState().setSummarizationThreshold(9_999_999);
    expect(useModelConfigStore.getState().summarizationThreshold).toBe(
      SUMMARY_THRESHOLD_MAX,
    );
    useModelConfigStore.getState().setSummarizationThreshold(1);
    expect(useModelConfigStore.getState().summarizationThreshold).toBe(
      SUMMARY_THRESHOLD_MIN,
    );
  });

  it("resetModel restores the local threshold and leaves the registry (incl. effort) alone", () => {
    useModelConfigStore.setState({
      summarizationThreshold: 300_000,
      registry: {
        active: "ollama",
        connections: [
          {
            id: "ollama",
            kind: "ollama",
            displayName: "Ollama",
            model: "llama3.2",
            hasKey: false,
            thinking: true,
            reasoningEffort: "low",
          },
        ],
      },
    });
    useModelConfigStore.getState().resetModel();
    const s = useModelConfigStore.getState();
    expect(s.summarizationThreshold).toBe(150_000);
    // The registry is backend-owned — reset leaves it (and per-connection effort) alone.
    expect(s.registry?.active).toBe("ollama");
    expect(activeConnection(s.registry)?.reasoningEffort).toBe("low");
  });
});

describe("model-config registry (mock IPC)", () => {
  // The store talks to a module-level MockIpc singleton, so state (active
  // connection, stored secrets, the bedrock auth mode mutated by a prior `it`)
  // bleeds across tests. Reset it to the seed before each test for isolation.
  beforeEach(async () => {
    for (const kind of ["apiKey", "secretAccessKey", "sessionToken"] as const) {
      await ipc.clearProviderSecret("bedrock", kind);
    }
    await ipc.upsertConnection({
      id: "bedrock",
      kind: "bedrock",
      displayName: "AWS Bedrock",
      model: "us.anthropic.claude-sonnet-4-0",
      hasKey: false,
      thinking: true,
      reasoningEffort: "low",
      region: "us-east-1",
      authMode: "profile",
      awsProfile: "bedrock-profile",
    });
    await ipc.setActiveConnection("candle-vllm");
    useModelConfigStore.setState({
      registry: null,
      modelsById: {},
      test: {},
      loading: false,
      saving: false,
      error: null,
    });
  });

  it("loads the registry and the active connection's models", async () => {
    await useModelConfigStore.getState().load();
    const s = useModelConfigStore.getState();
    const active = activeConnection(s.registry);
    expect(active?.kind).toBe("candleVllm");
    expect(active?.model).toBeTruthy();
    expect(s.modelsById[s.registry!.active].length).toBeGreaterThan(0);
  });

  it("includes a Bedrock connection with region + auth mode", async () => {
    await useModelConfigStore.getState().load();
    const bedrock = useModelConfigStore
      .getState()
      .registry?.connections.find((c) => c.kind === "bedrock");
    expect(bedrock?.region).toBe("us-east-1");
    expect(bedrock?.authMode).toBe("profile");
  });

  it("switches the default (active) connection through IPC", async () => {
    const { load, setActiveConnection } = useModelConfigStore.getState();
    await load();
    await setActiveConnection("ollama");
    expect(useModelConfigStore.getState().registry?.active).toBe("ollama");
  });

  it("reads the seeded active connection's reasoning effort (default medium)", async () => {
    await useModelConfigStore.getState().load();
    const active = activeConnection(useModelConfigStore.getState().registry);
    expect(active?.kind).toBe("candleVllm");
    expect(active?.reasoningEffort).toBe("medium");
  });

  it("setEffort persists a connection's reasoning effort via IPC", async () => {
    const { load, setEffort } = useModelConfigStore.getState();
    await load();
    await setEffort("ollama", "high");
    expect(
      useModelConfigStore
        .getState()
        .registry?.connections.find((c) => c.id === "ollama")?.reasoningEffort,
    ).toBe("high");
    // Survives a fresh reload from the backend (not just optimistic local state).
    useModelConfigStore.setState({ registry: null });
    await useModelConfigStore.getState().load();
    expect(
      useModelConfigStore
        .getState()
        .registry?.connections.find((c) => c.id === "ollama")?.reasoningEffort,
    ).toBe("high");
  });

  it("picking a default model also activates that connection", async () => {
    const { load, setDefaultModel } = useModelConfigStore.getState();
    await load();
    await setDefaultModel("ollama", "qwen2.5");
    const s = useModelConfigStore.getState();
    expect(s.registry?.active).toBe("ollama");
    expect(s.registry?.connections.find((c) => c.id === "ollama")?.model).toBe(
      "qwen2.5",
    );
  });

  it("stores a secret write-only and flips hasKey from the registry", async () => {
    const { load, setSecret, clearSecret } = useModelConfigStore.getState();
    await load();
    await setSecret("bedrock", "apiKey", "br-secret-value");
    expect(
      useModelConfigStore
        .getState()
        .registry?.connections.find((c) => c.id === "bedrock")?.hasKey,
    ).toBe(true);
    await clearSecret("bedrock", "apiKey");
    expect(
      useModelConfigStore
        .getState()
        .registry?.connections.find((c) => c.id === "bedrock")?.hasKey,
    ).toBe(false);
  });

  it("records a test-connection error when API-Key creds are missing", async () => {
    const { load, saveConnection, clearSecret, testConnection } =
      useModelConfigStore.getState();
    await load();
    const bedrock = useModelConfigStore
      .getState()
      .registry!.connections.find((c) => c.id === "bedrock")!;
    await clearSecret("bedrock", "apiKey");
    await saveConnection({ ...bedrock, authMode: "apiKey" });
    await testConnection("bedrock");
    const result = useModelConfigStore.getState().test["bedrock"];
    expect(result?.state).toBe("error");
  });
});
