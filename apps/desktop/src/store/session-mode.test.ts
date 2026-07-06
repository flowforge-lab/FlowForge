// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  MODE_ORDER,
  nextMode,
  useSessionModeStore,
} from "@/store/session-mode";

describe("nextMode / MODE_ORDER", () => {
  it("cycles Plan → Act → Auto → Plan", () => {
    expect(MODE_ORDER).toEqual(["plan", "act", "auto"]);
    expect(nextMode("plan")).toBe("act");
    expect(nextMode("act")).toBe("auto");
    expect(nextMode("auto")).toBe("plan");
  });
});

describe("useSessionModeStore", () => {
  beforeEach(() => {
    useSessionModeStore.setState({ modeBySession: {} });
    localStorage.clear();
  });

  it("resolve falls back to the default when no override is set", () => {
    expect(useSessionModeStore.getState().resolve("s1", "auto")).toBe("auto");
    useSessionModeStore.getState().setMode("s1", "plan");
    expect(useSessionModeStore.getState().resolve("s1", "auto")).toBe("plan");
  });

  it("cycleMode steps from the fallback, then from the stored value", () => {
    const { cycleMode } = useSessionModeStore.getState();
    cycleMode("s1", "auto"); // seeded from default auto → plan
    expect(useSessionModeStore.getState().modeBySession.s1).toBe("plan");
    cycleMode("s1", "auto"); // plan → act
    expect(useSessionModeStore.getState().modeBySession.s1).toBe("act");
    cycleMode("s1", "auto"); // act → auto
    expect(useSessionModeStore.getState().modeBySession.s1).toBe("auto");
  });

  it("keeps modes independent per session (per-pane)", () => {
    const { cycleMode } = useSessionModeStore.getState();
    cycleMode("s1", "auto"); // s1 → plan
    expect(useSessionModeStore.getState().modeBySession.s1).toBe("plan");
    // s2 untouched → still inherits the default.
    expect(useSessionModeStore.getState().resolve("s2", "auto")).toBe("auto");
  });

  it("persists overrides to localStorage under ff-session-mode", () => {
    useSessionModeStore.getState().setMode("s1", "act");
    const blob = JSON.parse(localStorage.getItem("ff-session-mode") ?? "{}");
    expect(blob.state.modeBySession.s1).toBe("act");
  });
});

// #789: the pill is authoritative but the running turn reads the persisted
// backend mode, so every change must mirror to `set_session_mode`.
describe("useSessionModeStore backend write-through", () => {
  beforeEach(() => {
    useSessionModeStore.setState({ modeBySession: {} });
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("setMode mirrors the chosen mode to the backend", () => {
    const spy = vi.spyOn(ipc, "setSessionMode").mockResolvedValue();
    useSessionModeStore.getState().setMode("s1", "plan");
    expect(spy).toHaveBeenCalledWith("s1", "plan");
  });

  it("cycleMode mirrors the resulting mode to the backend", () => {
    const spy = vi.spyOn(ipc, "setSessionMode").mockResolvedValue();
    useSessionModeStore.getState().cycleMode("s1", "auto"); // auto → plan
    expect(spy).toHaveBeenLastCalledWith("s1", "plan");
    useSessionModeStore.getState().cycleMode("s1", "auto"); // plan → act
    expect(spy).toHaveBeenLastCalledWith("s1", "act");
  });
});
