// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { stageFiles } from "./stage-files";
import { useComposerStore } from "@/store/composer";
import { useAttachRejectToastStore } from "@/store/attach-reject-toast";

const SID = "s1";
const OPEN = { visionGated: false, docGated: false };

function file(name: string, type: string): File {
  return new File(["x"], name, { type });
}

beforeEach(() => {
  useComposerStore.setState({ attachmentsBySession: {} });
  useAttachRejectToastStore.setState({ toasts: [] });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("stageFiles (#723)", () => {
  it("stages accepted files into the session's composer", async () => {
    const staged = stageFiles(SID, [file("a.png", "image/png")], OPEN);
    expect(staged).toBe(1);
    // fileToAttachment reads the file async; flush microtasks/FileReader.
    await vi.waitFor(() => {
      const atts = useComposerStore.getState().attachmentsBySession[SID] ?? [];
      expect(atts).toHaveLength(1);
      expect(atts[0].kind).toBe("image");
    });
    // No rejections → no toast.
    expect(useAttachRejectToastStore.getState().toasts).toHaveLength(0);
  });

  it("pushes a single reason-stating toast for rejected files", () => {
    const staged = stageFiles(
      SID,
      [
        file("a.zip", "application/zip"),
        file("b.bin", "application/octet-stream"),
      ],
      OPEN,
    );
    expect(staged).toBe(0);
    const toasts = useAttachRejectToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe("Skipped 2 files: unsupported type");
  });

  it("stages the good files and toasts about the rejected ones together", () => {
    const staged = stageFiles(
      SID,
      [file("a.png", "image/png"), file("c.mp4", "video/mp4")],
      OPEN,
    );
    expect(staged).toBe(1);
    expect(useAttachRejectToastStore.getState().toasts[0].message).toBe(
      "Skipped 1 file: unsupported type",
    );
  });

  it("stages nothing and explains the reason when the model is gated", () => {
    const staged = stageFiles(SID, [file("a.png", "image/png")], {
      visionGated: true,
      docGated: true,
    });
    expect(staged).toBe(0);
    expect(
      useComposerStore.getState().attachmentsBySession[SID],
    ).toBeUndefined();
    expect(useAttachRejectToastStore.getState().toasts[0].message).toBe(
      "This model can't accept images",
    );
  });
});
