// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  SUMMARY_THRESHOLD_MAX,
  SUMMARY_THRESHOLD_MIN,
  useModelConfigStore,
} from "@/store/model-config";

describe("model-config reasoning controls", () => {
  beforeEach(() => {
    useModelConfigStore.getState().resetModel();
  });
  afterEach(() => {
    localStorage.clear();
  });

  it("defaults medium effort and 150k threshold", () => {
    const s = useModelConfigStore.getState();
    expect(s.effort).toBe("medium");
    expect(s.summarizationThreshold).toBe(150_000);
  });

  it("updates the local reasoning controls", () => {
    useModelConfigStore.getState().setEffort("high");
    expect(useModelConfigStore.getState().effort).toBe("high");
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

  it("resetModel restores local defaults without touching provider", () => {
    useModelConfigStore.setState({
      effort: "low",
      summarizationThreshold: 300_000,
      provider: {
        kind: "ollama",
        model: "llama3.2",
        hasKey: false,
        thinking: false,
      },
    });
    useModelConfigStore.getState().resetModel();
    const s = useModelConfigStore.getState();
    expect(s.effort).toBe("medium");
    expect(s.summarizationThreshold).toBe(150_000);
    // Provider is backend-owned — reset leaves it alone.
    expect(s.provider?.kind).toBe("ollama");
    expect(s.provider?.thinking).toBe(false);
  });
});

describe("model-config provider load (mock IPC)", () => {
  it("loads the provider config and model list", async () => {
    await useModelConfigStore.getState().load();
    const s = useModelConfigStore.getState();
    expect(s.provider?.kind).toBe("candleVllm");
    expect(s.provider?.model).toBeTruthy();
    expect(s.provider?.thinking).toBe(true);
    expect(s.models.length).toBeGreaterThan(0);
  });

  it("round-trips a provider kind change through IPC", async () => {
    await useModelConfigStore.getState().load();
    await useModelConfigStore.getState().setKind("ollama");
    expect(useModelConfigStore.getState().provider?.kind).toBe("ollama");
  });

  it("switching kind keeps each connection's own model (no spillover)", async () => {
    const { load, setKind } = useModelConfigStore.getState();
    await load();
    // Normalize to candle-vLLM regardless of prior test order.
    if (useModelConfigStore.getState().provider?.kind !== "candleVllm") {
      await setKind("candleVllm");
    }
    expect(useModelConfigStore.getState().provider?.model).toBe(
      "Qwen3-4B-Instruct-2507",
    );

    await setKind("ollama");
    const s = useModelConfigStore.getState();
    expect(s.provider?.kind).toBe("ollama");
    // The Ollama connection's own model — not the candle model carried over.
    expect(s.provider?.model).toBe("llama3.2");
    expect(s.provider?.model).not.toBe("Qwen3-4B-Instruct-2507");
    expect(s.models).toContain("llama3.2");
  });

  it("persists the thinking toggle through IPC", async () => {
    await useModelConfigStore.getState().load();
    await useModelConfigStore.getState().setThinking(false);
    expect(useModelConfigStore.getState().provider?.thinking).toBe(false);
    await useModelConfigStore.getState().setThinking(true);
    expect(useModelConfigStore.getState().provider?.thinking).toBe(true);
  });
});
