// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Hoisted so the (hoisted) vi.mock factories can close over the spies.
const { exportSession, save, writeTextFile } = vi.hoisted(() => ({
  exportSession: vi.fn(),
  save: vi.fn(),
  writeTextFile: vi.fn(),
}));

vi.mock("@/lib/ipc", () => ({ ipc: { exportSession } }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save }));
vi.mock("@tauri-apps/plugin-fs", () => ({ writeTextFile }));

import { exportFilename, exportSessionToFile } from "@/lib/export-session";

// Force the Tauri branch (dialog + plugin-fs) so we exercise the path the issue
// specifies. `inTauri()` is evaluated per call, so setting this in beforeEach is enough.
beforeEach(() => {
  (globalThis.window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ =
    {};
  exportSession.mockReset().mockResolvedValue("EXPORTED");
  save.mockReset();
  writeTextFile.mockReset().mockResolvedValue(undefined);
});

afterEach(() => {
  delete (globalThis.window as { __TAURI_INTERNALS__?: unknown })
    .__TAURI_INTERNALS__;
});

describe("exportFilename", () => {
  it("slugifies the title and appends the format extension", () => {
    expect(exportFilename("My Great Chat!", "markdown")).toBe(
      "my-great-chat.md",
    );
    expect(exportFilename("My Great Chat!", "json")).toBe("my-great-chat.json");
  });

  it("falls back to 'session' when the title is empty/null", () => {
    expect(exportFilename(null, "markdown")).toBe("session.md");
    expect(exportFilename("   ", "json")).toBe("session.json");
  });
});

describe("exportSessionToFile (Tauri)", () => {
  it("exports with the right format and writes the dialog-chosen path", async () => {
    save.mockResolvedValue("/Users/me/out/my-chat.md");

    const result = await exportSessionToFile("sess-1", "My Chat", "markdown");

    // Dialog defaults to the slugified filename + the markdown filter.
    expect(save).toHaveBeenCalledWith({
      defaultPath: "my-chat.md",
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    // Backend serializes for the requested format…
    expect(exportSession).toHaveBeenCalledWith("sess-1", "markdown");
    // …and the returned string is written to the chosen path.
    expect(writeTextFile).toHaveBeenCalledWith(
      "/Users/me/out/my-chat.md",
      "EXPORTED",
    );
    expect(result).toEqual({
      status: "saved",
      path: "/Users/me/out/my-chat.md",
    });
  });

  it("passes the json format and filter", async () => {
    save.mockResolvedValue("/tmp/session.json");
    await exportSessionToFile("sess-1", null, "json");
    expect(save).toHaveBeenCalledWith({
      defaultPath: "session.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    expect(exportSession).toHaveBeenCalledWith("sess-1", "json");
  });

  it("cancels cleanly when the user dismisses the dialog — no export, no write", async () => {
    save.mockResolvedValue(null);
    const result = await exportSessionToFile("sess-1", "My Chat", "markdown");
    expect(result).toEqual({ status: "cancelled" });
    expect(exportSession).not.toHaveBeenCalled();
    expect(writeTextFile).not.toHaveBeenCalled();
  });
});
