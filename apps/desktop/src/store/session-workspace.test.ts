import { beforeEach, describe, expect, it } from "vitest";

import { useSessionWorkspaceStore } from "@/store/session-workspace";

// Backed by MockIpc in the test environment (not running inside Tauri). The
// mock returns a default workspace for unset sessions and round-trips a set.
describe("useSessionWorkspaceStore (#200)", () => {
  beforeEach(() => {
    useSessionWorkspaceStore.setState({ pathBySession: {} });
  });

  it("load caches the backend's workspace for a session", async () => {
    await useSessionWorkspaceStore.getState().load("sess-load");
    expect(useSessionWorkspaceStore.getState().get("sess-load")).toMatch(
      /projects\/flowforge$/,
    );
  });

  it("set caches the canonical path the backend returns (trimmed)", async () => {
    await useSessionWorkspaceStore.getState().set("sess-set", "  /tmp/proj  ");
    expect(useSessionWorkspaceStore.getState().get("sess-set")).toBe(
      "/tmp/proj",
    );
  });

  it("rejects an empty path and leaves the cache unchanged", async () => {
    await expect(
      useSessionWorkspaceStore.getState().set("sess-empty", "   "),
    ).rejects.toThrow();
    expect(
      useSessionWorkspaceStore.getState().get("sess-empty"),
    ).toBeUndefined();
  });

  it("keeps cache entries isolated per session", async () => {
    await useSessionWorkspaceStore.getState().set("sess-a", "/tmp/a");
    await useSessionWorkspaceStore.getState().load("sess-b");
    expect(useSessionWorkspaceStore.getState().get("sess-a")).toBe("/tmp/a");
    expect(useSessionWorkspaceStore.getState().get("sess-b")).toMatch(
      /projects\/flowforge$/,
    );
  });
});
