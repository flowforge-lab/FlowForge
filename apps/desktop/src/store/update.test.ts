// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { shouldPollUpdate, useUpdateStore } from "@/store/update";

afterEach(() => {
  useUpdateStore.setState({ status: null, installing: false });
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
