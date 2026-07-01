import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc provider config", () => {
  it("defaults to candle-vLLM with a model and no key", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getProviderConfig();
    expect(cfg.kind).toBe("candleVllm");
    expect(cfg.model).toBeTruthy();
    expect(cfg.hasKey).toBe(false);
    // Local kinds default reasoning off (#633).
    expect(cfg.thinking).toBe(false);
  });

  it("persists a provider/model change and echoes it back on reopen", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setProviderConfig(
      "ollama",
      undefined,
      "llama3.2",
      true,
    );
    expect(stored.kind).toBe("ollama");
    expect(stored.model).toBe("llama3.2");

    const reread = await ipc.getProviderConfig();
    expect(reread.kind).toBe("ollama");
    expect(reread.model).toBe("llama3.2");
  });

  it("treats a blank base url as unset", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setProviderConfig(
      "candleVllm",
      "   ",
      "Qwen3-4B",
      true,
    );
    expect(stored.baseUrl).toBeUndefined();
  });

  it("lists best-effort model ids for the active connection", async () => {
    const ipc = new MockIpc();
    const models = await ipc.listModels();
    expect(Array.isArray(models)).toBe(true);
    expect(models.length).toBeGreaterThan(0);
  });

  it("never reports a key (secrets are a later phase)", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setProviderConfig(
      "candleVllm",
      undefined,
      "m",
      false,
    );
    expect(stored.hasKey).toBe(false);
    expect(stored.thinking).toBe(false);
  });

  it("round-trips the thinking toggle", async () => {
    const ipc = new MockIpc();
    const off = await ipc.setProviderConfig(
      "candleVllm",
      undefined,
      "m",
      false,
    );
    expect(off.thinking).toBe(false);
    expect((await ipc.getProviderConfig()).thinking).toBe(false);
    const on = await ipc.setProviderConfig("candleVllm", undefined, "m", true);
    expect(on.thinking).toBe(true);
  });
});
