// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  progressPercent,
  shouldPollUpdate,
  activeUpdateChannel,
  useUpdateStore,
} from "@/store/update";
import { useExperimentalStore } from "@/store/experimental";

afterEach(() => {
  useUpdateStore.setState({ status: null, installing: false, progress: null });
  useExperimentalStore.getState().resetExperimental();
  vi.restoreAllMocks();
});

describe("useUpdateStore (#363)", () => {
  it("refresh() stores the check result", async () => {
    const spy = vi.spyOn(ipc, "checkForUpdates").mockResolvedValue({
      kind: "available",
      version: "9.9.9",
      notes: null,
    });
    await useUpdateStore.getState().refresh("github");
    expect(spy).toHaveBeenCalledWith("github");
    expect(useUpdateStore.getState().status).toEqual({
      kind: "available",
      version: "9.9.9",
      notes: null,
    });
  });

  it("refresh('github') swallows errors and leaves the previous status", async () => {
    useUpdateStore.setState({
      status: { kind: "upToDate", version: "0.1.0" },
    });
    vi.spyOn(ipc, "checkForUpdates").mockRejectedValue(new Error("offline"));
    await expect(
      useUpdateStore.getState().refresh("github"),
    ).resolves.toBeUndefined();
    expect(useUpdateStore.getState().status).toEqual({
      kind: "upToDate",
      version: "0.1.0",
    });
  });

  it("refresh('local') clears stale status when the dev feed is unreachable (#1033)", async () => {
    // A prior GitHub result must NOT stay pinned when the local channel fails —
    // this is the banner-stuck-on-0.1.0 bug.
    useUpdateStore.setState({
      status: { kind: "available", version: "0.1.0", notes: null },
    });
    vi.spyOn(ipc, "checkForUpdates").mockRejectedValue(
      new Error("connection refused"),
    );
    await expect(
      useUpdateStore.getState().refresh("local"),
    ).resolves.toBeUndefined();
    expect(useUpdateStore.getState().status).toBeNull();
  });

  it("install() calls installUpdate once with the channel and clears the flag", async () => {
    const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
    await useUpdateStore.getState().install("local");
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith("local");
    expect(useUpdateStore.getState().installing).toBe(false);
  });

  it("install() clears the installing flag even when the call rejects", async () => {
    vi.spyOn(ipc, "installUpdate").mockRejectedValue(new Error("boom"));
    await expect(useUpdateStore.getState().install("github")).rejects.toThrow(
      "boom",
    );
    expect(useUpdateStore.getState().installing).toBe(false);
  });

  it("dismiss() sets the dismissed flag", () => {
    useUpdateStore.setState({ dismissed: false });
    useUpdateStore.getState().dismiss();
    expect(useUpdateStore.getState().dismissed).toBe(true);
  });

  it("refresh() clears dismissed so a still-available update resurfaces (#565)", async () => {
    useUpdateStore.setState({
      status: { kind: "available", version: "9.9.9", notes: null },
      dismissed: true,
    });
    vi.spyOn(ipc, "checkForUpdates").mockResolvedValue({
      kind: "available",
      version: "9.9.9",
      notes: null,
    });
    await useUpdateStore.getState().refresh("github");
    expect(useUpdateStore.getState().dismissed).toBe(false);
  });

  describe("activeUpdateChannel (#1033)", () => {
    it("is 'github' when the localUpdateChannel flag is off", () => {
      useExperimentalStore.setState((s) => ({
        flags: { ...s.flags, localUpdateChannel: false },
      }));
      expect(activeUpdateChannel()).toBe("github");
    });

    it("is 'local' when the localUpdateChannel flag is on", () => {
      useExperimentalStore.setState((s) => ({
        flags: { ...s.flags, localUpdateChannel: true },
      }));
      expect(activeUpdateChannel()).toBe("local");
    });
  });

  describe("shouldPollUpdate (#567)", () => {
    it("does not poll in dev when the flag is off", () => {
      expect(shouldPollUpdate(false, true, false)).toBe(false);
    });

    it("polls in dev when the flag is on", () => {
      expect(shouldPollUpdate(false, true, true)).toBe(true);
    });

    it("always polls in prod (flag off)", () => {
      expect(shouldPollUpdate(true, false, false)).toBe(true);
    });

    it("always polls in prod (flag on, irrelevant)", () => {
      expect(shouldPollUpdate(true, false, true)).toBe(true);
    });
  });
});

describe("download progress (#566)", () => {
  it("setProgress stores progress; the terminal event clears it", () => {
    useUpdateStore.getState().setProgress({ downloaded: 512, total: 1024 });
    expect(useUpdateStore.getState().progress).toEqual({
      downloaded: 512,
      total: 1024,
    });
    // The `update:download-finished` listener clears progress (lib/events.ts).
    useUpdateStore.getState().setProgress(null);
    expect(useUpdateStore.getState().progress).toBeNull();
  });

  it("install() resets progress to null on start", async () => {
    useUpdateStore.setState({ progress: { downloaded: 1, total: 2 } });
    vi.spyOn(ipc, "installUpdate").mockResolvedValue();
    await useUpdateStore.getState().install("github");
    expect(useUpdateStore.getState().progress).toBeNull();
  });

  it("progressPercent is determinate when total is known", () => {
    expect(progressPercent({ downloaded: 256, total: 1024 })).toBe(25);
    expect(progressPercent({ downloaded: 1024, total: 1024 })).toBe(100);
  });

  it("progressPercent is null (indeterminate) without a total", () => {
    expect(progressPercent(null)).toBeNull();
    expect(progressPercent({ downloaded: 999, total: null })).toBeNull();
    // total 0 is a degenerate content length -> indeterminate, not a divide-by-zero.
    expect(progressPercent({ downloaded: 0, total: 0 })).toBeNull();
  });
});
