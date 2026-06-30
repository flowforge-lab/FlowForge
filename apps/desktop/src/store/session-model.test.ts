import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useSessionModelStore } from "@/store/session-model";

// These run against the in-browser MockIpc (the test build is VITE_FF_MOCK), so
// they also exercise the three new mock commands + the §11.2 resolver mirror.
// The mock's global default is the active connection candle-vLLM + its model.
// Caps are derived from the resolved (kind, model) (§11.3): candle-vLLM/Qwen is
// text-only, so both are false.
const GLOBAL_DEFAULT = {
  connection: "candle-vllm",
  model: "Qwen3-4B-Instruct-2507",
  supportsVision: false,
  supportsDocuments: false,
  // Mock has no live Ollama; the served-window contract (#602) carries nulls.
  contextWindow: null,
  trainedContextWindow: null,
  contextWindowSource: null,
};

beforeEach(() => {
  useSessionModelStore.setState({
    resolvedBySession: {},
    overrideBySession: {},
    unavailableBySession: {},
    servedWindowBySession: {},
  });
});

afterEach(async () => {
  vi.restoreAllMocks();
  // The MockIpc is a singleton — drop any overrides these tests set so they
  // don't bleed into the next case.
  for (const sid of ["s-load", "s-set", "s-clear"]) {
    await ipc.setSessionModelSelection(sid, null);
  }
});

describe("session-model store (#499)", () => {
  it("load resolves a fresh session to the global default, no override", async () => {
    await useSessionModelStore.getState().load("s-load");
    const s = useSessionModelStore.getState();
    expect(s.resolvedBySession["s-load"]).toEqual(GLOBAL_DEFAULT);
    expect(s.overrideBySession["s-load"]).toBeNull();
  });

  it("set writes the override and re-resolves to it", async () => {
    const sel = { connection: "ollama", model: "qwen2.5" };
    await useSessionModelStore.getState().set("s-set", sel);
    const s = useSessionModelStore.getState();
    expect(s.overrideBySession["s-set"]).toEqual(sel);
    // resolved folds in the derived caps (§11.3); ollama/qwen2.5 is text-only.
    expect(s.resolvedBySession["s-set"]).toEqual({
      ...sel,
      supportsVision: false,
      supportsDocuments: false,
      contextWindow: null,
      trainedContextWindow: null,
      contextWindowSource: null,
    });
  });

  it("clear drops the override and falls back to the default", async () => {
    await useSessionModelStore
      .getState()
      .set("s-clear", { connection: "ollama", model: "qwen2.5" });
    await useSessionModelStore.getState().clear("s-clear");
    const s = useSessionModelStore.getState();
    expect(s.overrideBySession["s-clear"]).toBeNull();
    expect(s.resolvedBySession["s-clear"]).toEqual(GLOBAL_DEFAULT);
  });

  it("set rejects an unknown connection and leaves the cache unchanged", async () => {
    await expect(
      useSessionModelStore
        .getState()
        .set("s-load", { connection: "nope", model: "x" }),
    ).rejects.toThrow(/unknown connection/);
    expect(useSessionModelStore.getState().resolvedBySession["s-load"]).toBe(
      undefined,
    );
  });

  it("marks the session unavailable (never rejects) when the backend resolver is absent", async () => {
    // Simulates the real app ahead of the backend half: the Phase D commands aren't
    // registered, so Tauri rejects. load must swallow it and flag unavailable.
    vi.spyOn(ipc, "resolveModelSelection").mockRejectedValue(
      new Error("command set_session_model_selection not found"),
    );
    await expect(
      useSessionModelStore.getState().load("s-load"),
    ).resolves.toBeUndefined();
    const s = useSessionModelStore.getState();
    expect(s.unavailableBySession["s-load"]).toBe(true);
    expect(s.resolvedBySession["s-load"]).toBeUndefined();
  });

  it("clears the unavailable flag once load succeeds again", async () => {
    useSessionModelStore.setState({
      unavailableBySession: { "s-load": true },
    });
    await useSessionModelStore.getState().load("s-load");
    expect(useSessionModelStore.getState().unavailableBySession["s-load"]).toBe(
      false,
    );
    expect(useSessionModelStore.getState().resolvedBySession["s-load"]).toEqual(
      GLOBAL_DEFAULT,
    );
  });

  it("resolves the phenotype tier when a session is bound but not overridden", async () => {
    // A real session bound to the `reviewer` phenotype (provider openai, model
    // gpt-4o) resolves to that pair via the phenotype tier — no session override.
    const session = await ipc.createSession();
    await ipc.setSessionPhenotype(session.id, "reviewer");
    await useSessionModelStore.getState().load(session.id);
    const s = useSessionModelStore.getState();
    expect(s.overrideBySession[session.id]).toBeNull();
    // openai/gpt-4o is vision-capable → derived supportsVision (§11.3).
    expect(s.resolvedBySession[session.id]).toEqual({
      connection: "openai",
      model: "gpt-4o",
      supportsVision: true,
      supportsDocuments: false,
      contextWindow: null,
      trainedContextWindow: null,
      contextWindowSource: null,
    });
  });

  it("populates servedWindowBySession from the resolver fields (#602)", async () => {
    vi.spyOn(ipc, "resolveModelSelection").mockResolvedValue({
      connection: "ollama",
      model: "qwen3.6",
      supportsVision: false,
      supportsDocuments: false,
      contextWindow: 131072,
      trainedContextWindow: 262144,
      contextWindowSource: "served",
    });
    await useSessionModelStore.getState().load("s-load");
    expect(
      useSessionModelStore.getState().servedWindowBySession["s-load"],
    ).toEqual({ window: 131072, trained: 262144, source: "served" });
  });

  it("leaves servedWindowBySession undefined when the backend reports no served window (#602)", async () => {
    // The null-coerce trap (the mapping must NOT yield `{window: 0, source:
    // "default"}` for null backend fields, which would render "serving 0k" + a
    // false amber under-fill dot in the chip).
    vi.spyOn(ipc, "resolveModelSelection").mockResolvedValue({
      connection: "candle-vllm",
      model: "Qwen3-4B-Instruct-2507",
      supportsVision: false,
      supportsDocuments: false,
      contextWindow: null,
      trainedContextWindow: null,
      contextWindowSource: null,
    });
    await useSessionModelStore.getState().load("s-load");
    expect(
      useSessionModelStore.getState().servedWindowBySession["s-load"],
    ).toBeUndefined();
  });
});
