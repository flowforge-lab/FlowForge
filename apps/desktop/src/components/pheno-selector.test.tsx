// @vitest-environment jsdom

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { PhenoSelector } from "@/components/pheno-selector";
import { useProfilesStore, type Profile } from "@/store/profiles";
import { ipc } from "@/lib/ipc";

const PROFILES: Profile[] = [
  {
    id: "default",
    name: "Default",
    description: "",
    skillCount: 0,
    locked: true,
    accent: "blue",
  },
  {
    id: "rust",
    name: "Rust",
    description: "",
    skillCount: 2,
    locked: false,
    accent: "violet",
  },
];

function seed(
  over: Partial<ReturnType<typeof useProfilesStore.getState>> = {},
) {
  useProfilesStore.setState({
    profiles: PROFILES,
    activeId: "default",
    loading: false,
    saving: false,
    error: null,
    ...over,
  });
}

// Radix DropdownMenu calls these pointer/scroll APIs that jsdom doesn't implement.
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("PhenoSelector (#245 2b)", () => {
  it("shows the active phenotype in the trigger", () => {
    seed({ activeId: "rust" });
    render(<PhenoSelector />);
    expect(screen.getByLabelText("Session phenotype").textContent).toContain(
      "Rust",
    );
  });

  it("falls back to a placeholder when no phenotype is active yet", () => {
    seed({ profiles: [], activeId: "default", loading: true });
    render(<PhenoSelector />);
    expect(screen.getByLabelText("Session phenotype").textContent).toContain(
      "Phenotype",
    );
  });

  it("opens, lists phenotypes, and switching one calls switchPhenotype", async () => {
    const spy = vi
      .spyOn(ipc, "switchPhenotype")
      .mockResolvedValue({ name: "rust", skills: [], mcpServers: [] });
    seed({ activeId: "default" });
    const user = userEvent.setup();
    render(<PhenoSelector />);

    await user.click(screen.getByLabelText("Session phenotype"));
    const rustItem = await screen.findByRole("menuitem", { name: /Rust/ });
    await user.click(rustItem);

    expect(spy).toHaveBeenCalledWith("rust");
  });

  it("renders a distinct error state when the phenotype load fails", async () => {
    vi.spyOn(ipc, "listPhenotypes").mockRejectedValue(new Error("boom"));
    useProfilesStore.setState({
      profiles: [],
      activeId: "default",
      loading: false,
      saving: false,
      error: null,
    });
    const user = userEvent.setup();
    render(<PhenoSelector />);

    // The lazy auto-load fails and records the error (and must not retry-loop).
    await waitFor(() => expect(useProfilesStore.getState().error).toBe("boom"));

    await user.click(screen.getByLabelText("Session phenotype"));
    const item = await screen.findByRole("menuitem", {
      name: /Failed to load/i,
    });
    expect(item).toBeTruthy();
    expect(item.getAttribute("title")).toBe("boom");
  });

  it("documents the v1 global behavior in the menu", async () => {
    seed();
    const user = userEvent.setup();
    render(<PhenoSelector />);

    await user.click(screen.getByLabelText("Session phenotype"));
    expect(await screen.findByText(/Applies to all panes/i)).toBeTruthy();
  });
});
