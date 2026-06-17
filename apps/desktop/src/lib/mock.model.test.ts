import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc provider config", () => {
  it("defaults to candle-vLLM with a model and no key", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getProviderConfig();
    expect(cfg.kind).toBe("candleVllm");
    expect(cfg.model).toBeTruthy();
    expect(cfg.hasKey).toBe(false);
  });

  it("persists a provider/model change and echoes it back on reopen", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setProviderConfig("ollama", undefined, "llama3.2");
    expect(stored.kind).toBe("ollama");
    expect(stored.model).toBe("llama3.2");

    const reread = await ipc.getProviderConfig();
    expect(reread.kind).toBe("ollama");
    expect(reread.model).toBe("llama3.2");
  });

  it("treats a blank base url as unset", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setProviderConfig("candleVllm", "   ", "Qwen3-4B");
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
    const stored = await ipc.setProviderConfig("candleVllm", undefined, "m");
    expect(stored.hasKey).toBe(false);
  });
});
