// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  clampNotebookPollInterval,
  EXPERIMENTAL_DEFAULTS,
  FLAG_IDS,
  NOTEBOOK_POLL_DEFAULT_MS,
  NOTEBOOK_POLL_MAX_MS,
  NOTEBOOK_POLL_MIN_MS,
  useExperimentalStore,
} from "@/store/experimental";

describe("useExperimentalStore", () => {
  beforeEach(() => {
    useExperimentalStore.setState({
      flags: { ...EXPERIMENTAL_DEFAULTS },
      notebookPollIntervalMs: NOTEBOOK_POLL_DEFAULT_MS,
    });
  });

  afterEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  it("defaults every flag off", () => {
    const { flags } = useExperimentalStore.getState();
    expect(FLAG_IDS.every((id) => flags[id] === false)).toBe(true);
  });

  it("setFlag toggles one flag without touching the others", () => {
    useExperimentalStore.getState().setFlag("spotlight", true);
    const { flags } = useExperimentalStore.getState();
    expect(flags.spotlight).toBe(true);
    expect(flags.preventSleep).toBe(false);
  });

  it("resetExperimental turns every flag back off", () => {
    const { setFlag } = useExperimentalStore.getState();
    setFlag("spotlight", true);
    setFlag("remoteExecution", true);

    useExperimentalStore.getState().resetExperimental();
    const { flags } = useExperimentalStore.getState();
    expect(FLAG_IDS.every((id) => flags[id] === false)).toBe(true);
  });

  it("persists flags to localStorage under ff-experimental", () => {
    useExperimentalStore.getState().setFlag("ownApiKey", true);
    const blob = JSON.parse(localStorage.getItem("ff-experimental") ?? "{}");
    expect(blob.state.flags.ownApiKey).toBe(true);
  });

  it("hydrates a pre-existing blob, filling new flags with their default", async () => {
    // A blob written before some flags existed — only one flag present.
    localStorage.setItem(
      "ff-experimental",
      JSON.stringify({ state: { flags: { spotlight: true } }, version: 0 }),
    );
    vi.resetModules();
    const { useExperimentalStore: fresh } =
      await import("@/store/experimental");
    const { flags } = fresh.getState();
    expect(flags.spotlight).toBe(true);
    // Missing keys hydrate to false, not undefined.
    expect(flags.preventSleep).toBe(false);
    expect(flags.ownApiKey).toBe(false);
  });

  it("defaults the notebook poll interval (#871 FE-1)", () => {
    expect(useExperimentalStore.getState().notebookPollIntervalMs).toBe(
      NOTEBOOK_POLL_DEFAULT_MS,
    );
  });

  it("setNotebookPollInterval clamps into the legal range", () => {
    const { setNotebookPollInterval } = useExperimentalStore.getState();
    setNotebookPollInterval(999);
    expect(useExperimentalStore.getState().notebookPollIntervalMs).toBe(
      NOTEBOOK_POLL_MIN_MS,
    );
    setNotebookPollInterval(999_999);
    expect(useExperimentalStore.getState().notebookPollIntervalMs).toBe(
      NOTEBOOK_POLL_MAX_MS,
    );
    // Non-finite input falls back to the default rather than clamping to 0 or
    // sticking around as NaN.
    setNotebookPollInterval(Number.NaN);
    expect(useExperimentalStore.getState().notebookPollIntervalMs).toBe(
      NOTEBOOK_POLL_DEFAULT_MS,
    );
  });

  it("resetExperimental also resets the poll interval", () => {
    const { setNotebookPollInterval, resetExperimental } =
      useExperimentalStore.getState();
    setNotebookPollInterval(2000);
    resetExperimental();
    expect(useExperimentalStore.getState().notebookPollIntervalMs).toBe(
      NOTEBOOK_POLL_DEFAULT_MS,
    );
  });

  it("clamps a hydrated poll interval that landed outside the legal range", async () => {
    localStorage.setItem(
      "ff-experimental",
      JSON.stringify({
        state: { flags: {}, notebookPollIntervalMs: 99 },
        version: 0,
      }),
    );
    vi.resetModules();
    const { useExperimentalStore: fresh } =
      await import("@/store/experimental");
    expect(fresh.getState().notebookPollIntervalMs).toBe(NOTEBOOK_POLL_MIN_MS);
    // Also covers the NaN edge case via the helper.
    expect(clampNotebookPollInterval(Number.NaN)).toBe(
      NOTEBOOK_POLL_DEFAULT_MS,
    );
  });
});
