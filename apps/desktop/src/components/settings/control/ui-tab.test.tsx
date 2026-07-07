// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { UiTab } from "./ui-tab";
import { useControlConfigStore } from "@/store/control-config";
import { CONTROL_DEFAULTS } from "@/lib/control";

// The UI tab dynamically imports the native dialog (#803); mock it so tests can
// drive the Tauri branch without a real webview.
const { open } = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open }));

function seedConfig() {
  useControlConfigStore.setState({
    config: {
      ...CONTROL_DEFAULTS,
      ui: {
        accentColor: "",
        logoPath: "",
        faviconPath: "",
        contextualGreeting: true,
      },
    },
    saving: false,
    error: null,
  });
}

/** The logo row is first, favicon second — both buttons read "Choose file…". */
function chooseButton(which: "logo" | "favicon") {
  const btns = screen.getAllByRole("button", { name: /choose file/i });
  return which === "logo" ? btns[0] : btns[1];
}

describe("UiTab logo/favicon picker (#803)", () => {
  beforeEach(() => {
    seedConfig();
    open.mockReset();
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    delete (globalThis.window as unknown as Record<string, unknown>)
      .__TAURI_INTERNALS__;
  });

  it("opens the native image dialog and persists the picked path (Tauri)", async () => {
    (
      globalThis.window as unknown as Record<string, unknown>
    ).__TAURI_INTERNALS__ = {};
    open.mockResolvedValue("/Users/me/Pictures/logo.png");
    render(<UiTab />);

    await userEvent.click(chooseButton("logo"));

    expect(open).toHaveBeenCalledWith({
      multiple: false,
      filters: [
        { name: "Image", extensions: ["png", "jpg", "jpeg", "svg", "ico"] },
      ],
    });
    expect(await screen.findByText("/Users/me/Pictures/logo.png")).toBeTruthy();
  });

  it("leaves the value unchanged when the dialog is cancelled (Tauri)", async () => {
    (
      globalThis.window as unknown as Record<string, unknown>
    ).__TAURI_INTERNALS__ = {};
    open.mockResolvedValue(null);
    render(<UiTab />);

    await userEvent.click(chooseButton("logo"));

    expect(open).toHaveBeenCalledOnce();
    // The logo row still shows the empty-state, no path chip.
    expect(screen.getAllByText("No file").length).toBe(2);
    expect(useControlConfigStore.getState().config?.ui.logoPath).toBe("");
  });

  it("falls back to a hand-typed path in the web/mock build", async () => {
    // No __TAURI_INTERNALS__ → the prompt fallback runs, not the native dialog.
    vi.spyOn(window, "prompt").mockReturnValue("  ~/art/favicon.ico  ");
    render(<UiTab />);

    await userEvent.click(chooseButton("favicon"));

    expect(open).not.toHaveBeenCalled();
    // Trimmed before persisting.
    expect(await screen.findByText("~/art/favicon.ico")).toBeTruthy();
  });

  it("no-ops when the hand-typed prompt is cancelled (web/mock)", async () => {
    vi.spyOn(window, "prompt").mockReturnValue(null);
    render(<UiTab />);

    await userEvent.click(chooseButton("favicon"));

    expect(screen.getAllByText("No file").length).toBe(2);
    expect(useControlConfigStore.getState().config?.ui.faviconPath).toBe("");
  });
});
