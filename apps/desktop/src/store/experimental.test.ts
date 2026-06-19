// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  EXPERIMENTAL_DEFAULTS,
  FLAG_IDS,
  useExperimentalStore,
} from "@/store/experimental";

describe("useExperimentalStore", () => {
  beforeEach(() => {
    useExperimentalStore.setState({ flags: { ...EXPERIMENTAL_DEFAULTS } });
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
});
