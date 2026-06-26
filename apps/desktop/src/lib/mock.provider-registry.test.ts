import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { ProviderConnection } from "../bindings";

const conn = (over: Partial<ProviderConnection> = {}): ProviderConnection => ({
  id: "",
  kind: "candleVllm",
  displayName: "New Connection",
  model: "some-model",
  hasKey: false,
  thinking: true,
  reasoningEffort: "medium",
  supportsVision: false,
  supportsDocuments: false,
  ...over,
});

describe("MockIpc provider connection registry", () => {
  it("seeds candle-vLLM (active), a keyless Ollama, AWS Bedrock, SiliconFlow, and OpenAI", async () => {
    const ipc = new MockIpc();
    const reg = await ipc.getProviderRegistry();
    expect(reg.active).toBe("candle-vllm");
    expect(reg.connections.map((c) => c.id)).toEqual([
      "candle-vllm",
      "ollama",
      "bedrock",
      "siliconflow",
      "openai",
    ]);
    expect(reg.connections.every((c) => c.hasKey === false)).toBe(true);
    const bedrock = reg.connections.find((c) => c.id === "bedrock");
    expect(bedrock?.kind).toBe("bedrock");
    expect(bedrock?.region).toBe("us-east-1");
    expect(bedrock?.authMode).toBe("profile");
    const sf = reg.connections.find((c) => c.id === "siliconflow");
    expect(sf?.kind).toBe("siliconFlow");
    expect(sf?.displayName).toBe("SiliconFlow");
    // Unset baseUrl → defaults to the global `.com` endpoint.
    expect(sf?.baseUrl).toBeUndefined();
    const openai = reg.connections.find((c) => c.id === "openai");
    expect(openai?.kind).toBe("openai");
    expect(openai?.displayName).toBe("OpenAI");
    // Unset baseUrl → defaults to https://api.openai.com/v1.
    expect(openai?.baseUrl).toBeUndefined();
  });

  it("getProviderRegistry returns a copy callers cannot mutate", async () => {
    const ipc = new MockIpc();
    const reg = await ipc.getProviderRegistry();
    reg.connections.pop();
    reg.active = "tampered";
    const fresh = await ipc.getProviderRegistry();
    expect(fresh.connections).toHaveLength(5);
    expect(fresh.active).toBe("candle-vllm");
  });

  it("switches the active connection", async () => {
    const ipc = new MockIpc();
    await ipc.setActiveConnection("ollama");
    expect((await ipc.getProviderRegistry()).active).toBe("ollama");
  });

  it("rejects activating an unknown connection", async () => {
    const ipc = new MockIpc();
    await expect(ipc.setActiveConnection("nope")).rejects.toThrow(/unknown/);
  });

  it("upsert edits an existing connection in place", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.upsertConnection(
      conn({ id: "ollama", kind: "ollama", model: "qwen2.5" }),
    );
    expect(stored.model).toBe("qwen2.5");
    const reg = await ipc.getProviderRegistry();
    expect(reg.connections).toHaveLength(5);
    expect(reg.connections.find((c) => c.id === "ollama")?.model).toBe(
      "qwen2.5",
    );
  });

  it("upsert appends a new connection and derives a slug id when blank", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.upsertConnection(
      conn({ vendor: "OpenRouter", displayName: "OpenRouter" }),
    );
    expect(stored.id).toBe("openrouter");
    expect((await ipc.getProviderRegistry()).connections).toHaveLength(6);
  });

  it("dedupes derived ids with a numeric suffix", async () => {
    const ipc = new MockIpc();
    // "ollama" is already seeded, so derived ids start at -2.
    const first = await ipc.upsertConnection(conn({ displayName: "Ollama" }));
    const second = await ipc.upsertConnection(conn({ displayName: "Ollama" }));
    expect(first.id).toBe("ollama-2");
    expect(second.id).toBe("ollama-3");
  });

  it("removes a connection and reassigns active when needed", async () => {
    const ipc = new MockIpc();
    await ipc.setActiveConnection("ollama");
    await ipc.removeConnection("ollama");
    const reg = await ipc.getProviderRegistry();
    expect(reg.connections.map((c) => c.id)).toEqual([
      "candle-vllm",
      "bedrock",
      "siliconflow",
      "openai",
    ]);
    expect(reg.active).toBe("candle-vllm");
  });

  it("rejects removing the last connection", async () => {
    const ipc = new MockIpc();
    await ipc.removeConnection("ollama");
    await ipc.removeConnection("bedrock");
    await ipc.removeConnection("siliconflow");
    await ipc.removeConnection("openai");
    await expect(ipc.removeConnection("candle-vllm")).rejects.toThrow(/last/);
  });

  it("listModels defaults to the active connection and accepts an explicit id", async () => {
    const ipc = new MockIpc();
    expect(await ipc.listModels()).toContain("Qwen3-4B-Instruct-2507");
    expect(await ipc.listModels("ollama")).toEqual([
      "llama3.2",
      "qwen2.5",
      "mistral",
    ]);
    expect(await ipc.listModels("unknown")).toEqual([]);
  });

  it("lists Bedrock inference-profile ids for the bedrock connection", async () => {
    const ipc = new MockIpc();
    const models = await ipc.listModels("bedrock");
    expect(models).toContain("us.anthropic.claude-sonnet-4-0");
    expect(models).toContain("amazon.nova-pro-v1:0");
  });

  it("stores a secret write-only and flips hasKey, then clears it", async () => {
    const ipc = new MockIpc();
    await ipc.setProviderSecret("bedrock", "apiKey", "br-abc123");
    expect(
      (await ipc.getProviderRegistry()).connections.find(
        (c) => c.id === "bedrock",
      )?.hasKey,
    ).toBe(true);
    await ipc.clearProviderSecret("bedrock", "apiKey");
    expect(
      (await ipc.getProviderRegistry()).connections.find(
        (c) => c.id === "bedrock",
      )?.hasKey,
    ).toBe(false);
  });

  it("rejects writing a secret to an unknown connection", async () => {
    const ipc = new MockIpc();
    await expect(ipc.setProviderSecret("nope", "apiKey", "x")).rejects.toThrow(
      /unknown/,
    );
  });

  it("test_connection passes for a local kind and a profile-mode Bedrock", async () => {
    const ipc = new MockIpc();
    await expect(ipc.testConnection("candle-vllm")).resolves.toBeUndefined();
    // Seeded Bedrock is profile mode → resolves from ~/.aws, no secret needed.
    await expect(ipc.testConnection("bedrock")).resolves.toBeUndefined();
  });

  it("test_connection fails for API-Key Bedrock with no stored key", async () => {
    const ipc = new MockIpc();
    const bedrock = (await ipc.getProviderRegistry()).connections.find(
      (c) => c.id === "bedrock",
    )!;
    await ipc.upsertConnection({ ...bedrock, authMode: "apiKey" });
    await expect(ipc.testConnection("bedrock")).rejects.toThrow(/API key/);
    // Once the key is stored, the probe passes.
    await ipc.setProviderSecret("bedrock", "apiKey", "br-abc123");
    await expect(ipc.testConnection("bedrock")).resolves.toBeUndefined();
  });

  it("test_connection fails for IAM-Keys Bedrock until both key id and secret are set", async () => {
    const ipc = new MockIpc();
    const bedrock = (await ipc.getProviderRegistry()).connections.find(
      (c) => c.id === "bedrock",
    )!;
    // IAM mode with neither the access key id nor the secret access key.
    await ipc.upsertConnection({
      ...bedrock,
      authMode: "iamKeys",
      accessKeyId: undefined,
    });
    await expect(ipc.testConnection("bedrock")).rejects.toThrow(/IAM keys/);

    // Access key id alone is still incomplete (secret access key missing).
    await ipc.upsertConnection({
      ...bedrock,
      authMode: "iamKeys",
      accessKeyId: "AKIAEXAMPLE",
    });
    await expect(ipc.testConnection("bedrock")).rejects.toThrow(/IAM keys/);

    // Both present → the probe passes.
    await ipc.setProviderSecret("bedrock", "secretAccessKey", "shhh");
    await expect(ipc.testConnection("bedrock")).resolves.toBeUndefined();
  });

  it("lists SiliconFlow model ids only once an API key is stored", async () => {
    const ipc = new MockIpc();
    // Without a key the probe 401s — `list_models` returns empty, not the catalog
    // (#486: lets the FE's post-credentials recovery be exercised offline).
    expect(await ipc.listModels("siliconflow")).toEqual([]);
    await ipc.setProviderSecret("siliconflow", "apiKey", "sk-abc123");
    const models = await ipc.listModels("siliconflow");
    expect(models).toContain("deepseek-ai/DeepSeek-V3");
    expect(models).toContain("Qwen/Qwen3-8B");
  });

  it("test_connection fails for SiliconFlow until an API key is stored", async () => {
    const ipc = new MockIpc();
    await expect(ipc.testConnection("siliconflow")).rejects.toThrow(
      /SiliconFlow API key/,
    );
    await ipc.setProviderSecret("siliconflow", "apiKey", "sk-abc123");
    await expect(ipc.testConnection("siliconflow")).resolves.toBeUndefined();
  });

  it("lists OpenAI model ids only once an API key is stored", async () => {
    const ipc = new MockIpc();
    expect(await ipc.listModels("openai")).toEqual([]);
    await ipc.setProviderSecret("openai", "apiKey", "sk-abc123");
    const models = await ipc.listModels("openai");
    expect(models).toContain("gpt-4o");
    expect(models).toContain("o3");
  });

  it("test_connection fails for OpenAI until an API key is stored", async () => {
    const ipc = new MockIpc();
    await expect(ipc.testConnection("openai")).rejects.toThrow(
      /OpenAI API key/,
    );
    await ipc.setProviderSecret("openai", "apiKey", "sk-abc123");
    await expect(ipc.testConnection("openai")).resolves.toBeUndefined();
  });

  it("provider_secret_presence returns stored kinds in ALL order, throws on unknown", async () => {
    const ipc = new MockIpc();
    expect(await ipc.providerSecretPresence("bedrock")).toEqual([]);
    // Store out of order; presence reports in SecretKind::ALL order.
    await ipc.setProviderSecret("bedrock", "sessionToken", "tok");
    await ipc.setProviderSecret("bedrock", "apiKey", "br-key");
    expect(await ipc.providerSecretPresence("bedrock")).toEqual([
      "apiKey",
      "sessionToken",
    ]);
    await ipc.clearProviderSecret("bedrock", "apiKey");
    expect(await ipc.providerSecretPresence("bedrock")).toEqual([
      "sessionToken",
    ]);
    await expect(ipc.providerSecretPresence("nope")).rejects.toThrow(/unknown/);
  });

  it("resolved_bedrock_auth returns the explicit pin and null for non-Bedrock/unknown", async () => {
    const ipc = new MockIpc();
    // Non-Bedrock kinds and unknown ids resolve to null.
    expect(await ipc.resolvedBedrockAuth("candle-vllm")).toBeNull();
    expect(await ipc.resolvedBedrockAuth("nope")).toBeNull();
    // An explicit pin is returned verbatim, regardless of what's stored.
    const bedrock = (await ipc.getProviderRegistry()).connections.find(
      (c) => c.id === "bedrock",
    )!;
    await ipc.upsertConnection({ ...bedrock, authMode: "iamKeys" });
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("iamKeys");
  });

  it("resolved_bedrock_auth (auto) follows API key > profile > IAM keys, falling back to profile", async () => {
    const ipc = new MockIpc();
    const bedrock = (await ipc.getProviderRegistry()).connections.find(
      (c) => c.id === "bedrock",
    )!;
    const setAuto = (over: Partial<typeof bedrock> = {}) =>
      ipc.upsertConnection({ ...bedrock, authMode: "auto", ...over });

    // Nothing configured (seed profile cleared) → profile fallback.
    await setAuto({ awsProfile: undefined });
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("profile");

    // IAM keys only (access key id + secret access key, no profile) → iamKeys.
    await setAuto({ accessKeyId: "AKIAEXAMPLE", awsProfile: undefined });
    await ipc.setProviderSecret("bedrock", "secretAccessKey", "shhh");
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("iamKeys");

    // Add a profile → profile wins over IAM keys.
    await setAuto({ accessKeyId: "AKIAEXAMPLE", awsProfile: "dev" });
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("profile");

    // Add an API key → API key wins over everything.
    await ipc.setProviderSecret("bedrock", "apiKey", "br-key");
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("apiKey");
  });

  it("test_connection (auto) resolves a credential by precedence before probing", async () => {
    const ipc = new MockIpc();
    const bedrock = (await ipc.getProviderRegistry()).connections.find(
      (c) => c.id === "bedrock",
    )!;
    // Auto with nothing stored resolves to profile → the probe passes.
    await ipc.upsertConnection({ ...bedrock, authMode: "auto" });
    await expect(ipc.testConnection("bedrock")).resolves.toBeUndefined();
    // Auto with a stored API key resolves to apiKey → still passes.
    await ipc.setProviderSecret("bedrock", "apiKey", "br-key");
    await expect(ipc.testConnection("bedrock")).resolves.toBeUndefined();
    expect(await ipc.resolvedBedrockAuth("bedrock")).toBe("apiKey");
  });

  it("getProviderConfig/setProviderConfig shim the active connection", async () => {
    const ipc = new MockIpc();
    await ipc.setProviderConfig(
      "candleVllm",
      "http://localhost:9000/v1",
      "m",
      true,
    );
    const cfg = await ipc.getProviderConfig();
    expect(cfg.baseUrl).toBe("http://localhost:9000/v1");
    expect(cfg.model).toBe("m");
    // The mutation lands on the active connection in the registry.
    const reg = await ipc.getProviderRegistry();
    expect(reg.connections.find((c) => c.id === "candle-vllm")?.model).toBe(
      "m",
    );
  });
});
