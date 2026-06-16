import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { ProviderConnection } from "../bindings";

const conn = (over: Partial<ProviderConnection> = {}): ProviderConnection => ({
  id: "",
  kind: "candleVllm",
  displayName: "New Connection",
  model: "some-model",
  hasKey: false,
  ...over,
});

describe("MockIpc provider connection registry", () => {
  it("seeds candle-vLLM (active) plus a keyless Ollama", async () => {
    const ipc = new MockIpc();
    const reg = await ipc.getProviderRegistry();
    expect(reg.active).toBe("candle-vllm");
    expect(reg.connections.map((c) => c.id)).toEqual(["candle-vllm", "ollama"]);
    expect(reg.connections.every((c) => c.hasKey === false)).toBe(true);
  });

  it("getProviderRegistry returns a copy callers cannot mutate", async () => {
    const ipc = new MockIpc();
    const reg = await ipc.getProviderRegistry();
    reg.connections.pop();
    reg.active = "tampered";
    const fresh = await ipc.getProviderRegistry();
    expect(fresh.connections).toHaveLength(2);
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
    expect(reg.connections).toHaveLength(2);
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
    expect((await ipc.getProviderRegistry()).connections).toHaveLength(3);
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
    expect(reg.connections.map((c) => c.id)).toEqual(["candle-vllm"]);
    expect(reg.active).toBe("candle-vllm");
  });

  it("rejects removing the last connection", async () => {
    const ipc = new MockIpc();
    await ipc.removeConnection("ollama");
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

  it("getProviderConfig/setProviderConfig shim the active connection", async () => {
    const ipc = new MockIpc();
    await ipc.setProviderConfig("candleVllm", "http://localhost:9000/v1", "m");
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
