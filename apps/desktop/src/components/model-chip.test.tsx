// @vitest-environment jsdom

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { ModelChip } from "@/components/model-chip";
import { ipc } from "@/lib/ipc";
import { useSessionModelStore } from "@/store/session-model";

// Radix DropdownMenu calls pointer/scroll APIs jsdom lacks.
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
});

afterEach(async () => {
  cleanup();
  vi.restoreAllMocks();
  for (const sid of ["s1", "s2", "sA", "sB"]) {
    await ipc.setSessionModelSelection(sid, null);
  }
  useSessionModelStore.setState({
    resolvedBySession: {},
    overrideBySession: {},
    unavailableBySession: {},
    servedWindowBySession: {},
  });
});

const DEFAULT_MODEL = "Qwen3-4B-Instruct-2507"; // mock global active (candle-vLLM)

describe("ModelChip (#499)", () => {
  it("shows the resolved model for an inherited (un-overridden) session", async () => {
    render(<ModelChip sessionId="s1" />);
    expect(await screen.findByText(DEFAULT_MODEL)).not.toBeNull();
    const trigger = screen.getByLabelText("Session model");
    expect(trigger.getAttribute("title")).toContain("(inherited)");
  });

  it("reflects a session override in the label and title", async () => {
    await ipc.setSessionModelSelection("s2", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(<ModelChip sessionId="s2" />);
    expect(await screen.findByText("qwen2.5")).not.toBeNull();
    const trigger = screen.getByLabelText("Session model");
    expect(trigger.getAttribute("title")).toContain("(session override)");
  });

  it("disables the clear item when the session is not overridden", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await userEvent.click(screen.getByLabelText("Session model"));
    const clear = await screen.findByText("Use phenotype / global default");
    // Radix marks a disabled item with aria-disabled on the menuitem.
    expect(
      clear.closest('[role="menuitem"]')?.getAttribute("aria-disabled"),
    ).toBe("true");
  });

  it("clears an override back to the default when the clear item is chosen", async () => {
    await ipc.setSessionModelSelection("s2", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(<ModelChip sessionId="s2" />);
    await screen.findByText("qwen2.5");
    await userEvent.click(screen.getByLabelText("Session model"));
    await userEvent.click(
      await screen.findByText("Use phenotype / global default"),
    );
    await waitFor(() => expect(screen.getByText(DEFAULT_MODEL)).not.toBeNull());
  });

  it("hides the chip when the backend resolver is unavailable (merges ahead of backend)", async () => {
    // The Phase D commands aren't registered in the real app yet, so the resolver
    // rejects; the chip must hide rather than spin forever or leak a rejection.
    vi.spyOn(ipc, "resolveModelSelection").mockRejectedValue(
      new Error("command not found"),
    );
    render(<ModelChip sessionId="s1" />);
    await waitFor(() =>
      expect(screen.queryByLabelText("Session model")).toBeNull(),
    );
  });

  it("is per-session: two panes show their own resolved model", async () => {
    await ipc.setSessionModelSelection("sA", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(
      <>
        <ModelChip sessionId="sA" />
        <ModelChip sessionId="sB" />
      </>,
    );
    expect(await screen.findByText("qwen2.5")).not.toBeNull();
    expect(await screen.findByText(DEFAULT_MODEL)).not.toBeNull();
  });
});

describe("ModelChip served-window display (#602)", () => {
  const WARNING = /context window not detected/i;

  // Spy on the resolver so the load() in ModelChip mount forwards the desired
  // served-window fields, mirroring the production BE half (#602).
  function mockResolved(extra: {
    contextWindow: number | null;
    trainedContextWindow: number | null;
    contextWindowSource: "explicit" | "served" | "default" | null;
  }) {
    vi.spyOn(ipc, "resolveModelSelection").mockImplementation(async () => ({
      connection: "candle",
      model: DEFAULT_MODEL,
      supportsVision: false,
      supportsDocuments: false,
      ...extra,
    }));
  }

  it("shows the served window + source in the dropdown when present", async () => {
    mockResolved({
      contextWindow: 131072,
      trainedContextWindow: 262144,
      contextWindowSource: "served",
    });
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await userEvent.click(screen.getByLabelText("Session model"));

    expect(await screen.findByText(/serving 128k/i)).not.toBeNull();
    expect(screen.getByText(/trained 256k/i)).not.toBeNull();
    expect(screen.getByText(/auto-detected from server/i)).not.toBeNull();
    // Not the conservative-default fallback, so no under-fill warning dot.
    expect(screen.queryByLabelText(WARNING)).toBeNull();
  });

  it("shows an always-visible warning dot on the trigger for the default fallback", async () => {
    mockResolved({
      contextWindow: 32000,
      trainedContextWindow: null,
      contextWindowSource: "default",
    });
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await waitFor(() => expect(screen.getByLabelText(WARNING)).not.toBeNull());
  });

  it("renders neither the readout nor the warning dot when no served window is known", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    expect(screen.queryByLabelText(WARNING)).toBeNull();
    await userEvent.click(screen.getByLabelText("Session model"));
    expect(screen.queryByText(/serving/i)).toBeNull();
  });
});
