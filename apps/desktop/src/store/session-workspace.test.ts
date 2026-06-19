import { beforeEach, describe, expect, it } from "vitest";

import { useSessionWorkspaceStore } from "@/store/session-workspace";

// Backed by MockIpc in the test environment (not running inside Tauri). The
// mock returns a default workspace for unset sessions and round-trips a set;
// it never resolves a git branch (no real filesystem), so `gitBranch` is null.
describe("useSessionWorkspaceStore (#200, #211)", () => {
  beforeEach(() => {
    useSessionWorkspaceStore.setState({ bySession: {}, recents: [] });
  });

  it("load caches the backend's workspace for a session", async () => {
    await useSessionWorkspaceStore.getState().load("sess-load");
    const ws = useSessionWorkspaceStore.getState().get("sess-load");
    expect(ws?.path).toMatch(/projects\/flowforge$/);
    expect(ws?.gitBranch).toBeNull();
  });

  it("set caches the canonical path the backend returns (trimmed)", async () => {
    await useSessionWorkspaceStore.getState().set("sess-set", "  /tmp/proj  ");
    expect(useSessionWorkspaceStore.getState().get("sess-set")?.path).toBe(
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

  it("tracks distinct recents, most-recent-first, de-duped (#210)", async () => {
    const store = useSessionWorkspaceStore.getState();
    await store.set("sess-r", "/tmp/one");
    await store.set("sess-r", "/tmp/two");
    await store.set("sess-r", "/tmp/one"); // re-select moves it back to front
    expect(useSessionWorkspaceStore.getState().recents).toEqual([
      "/tmp/one",
      "/tmp/two",
    ]);
  });

  it("keeps cache entries isolated per session", async () => {
    await useSessionWorkspaceStore.getState().set("sess-a", "/tmp/a");
    await useSessionWorkspaceStore.getState().load("sess-b");
    expect(useSessionWorkspaceStore.getState().get("sess-a")?.path).toBe(
      "/tmp/a",
    );
    expect(useSessionWorkspaceStore.getState().get("sess-b")?.path).toMatch(
      /projects\/flowforge$/,
    );
  });
});
