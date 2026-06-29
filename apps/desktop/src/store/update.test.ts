// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  progressPercent,
  shouldPollUpdate,
  useUpdateStore,
} from "@/store/update";

afterEach(() => {
  useUpdateStore.setState({ status: null, installing: false, progress: null });
  vi.restoreAllMocks();
});

describe("useUpdateStore (#363)", () => {
  it("refresh() stores the check result", async () => {
    vi.spyOn(ipc, "checkForUpdates").mockResolvedValue({
      kind: "available",
      version: "9.9.9",
      notes: null,
    });
    await useUpdateStore.getState().refresh();
    expect(useUpdateStore.getState().status).toEqual({
      kind: "available",
      version: "9.9.9",
      notes: null,
    });
  });

  it("refresh() swallows errors and leaves the previous status", async () => {
    useUpdateStore.setState({
      status: { kind: "upToDate", version: "0.1.0" },
    });
    vi.spyOn(ipc, "checkForUpdates").mockRejectedValue(new Error("offline"));
    await expect(useUpdateStore.getState().refresh()).resolves.toBeUndefined();
    expect(useUpdateStore.getState().status).toEqual({
      kind: "upToDate",
      version: "0.1.0",
    });
  });

  it("install() calls installUpdate once and clears the installing flag", async () => {
    const spy = vi.spyOn(ipc, "installUpdate").mockResolvedValue();
    await useUpdateStore.getState().install();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(useUpdateStore.getState().installing).toBe(false);
  });

  it("install() clears the installing flag even when the call rejects", async () => {
    vi.spyOn(ipc, "installUpdate").mockRejectedValue(new Error("boom"));
    await expect(useUpdateStore.getState().install()).rejects.toThrow("boom");
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
    await useUpdateStore.getState().refresh();
    expect(useUpdateStore.getState().dismissed).toBe(false);
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
    await useUpdateStore.getState().install();
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
