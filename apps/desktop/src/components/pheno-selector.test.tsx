// @vitest-environment jsdom

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { PhenoSelector } from "@/components/pheno-selector";
import { useProfilesStore, type Profile } from "@/store/profiles";
import { useChatStore } from "@/store/chat";
import { ipc } from "@/lib/ipc";
import type { Session } from "@/bindings";

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
    id: "orchestrator",
    name: "Orchestrator",
    description: "",
    skillCount: 3,
    locked: false,
    accent: "violet",
  },
  {
    id: "rust",
    name: "Rust",
    description: "",
    skillCount: 2,
    locked: false,
    accent: "emerald",
  },
];

/** A minimal session record — the selector only reads `id` and `phenotype`. */
function session(id: string, phenotype?: string): Session {
  return {
    id,
    goal: null,
    title: null,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    phenotype,
  };
}

function seed(
  over: Partial<ReturnType<typeof useProfilesStore.getState>> = {},
) {
  useProfilesStore.setState({
    profiles: PROFILES,
    activeId: "orchestrator",
    loading: false,
    saving: false,
    error: null,
    ...over,
  });
}

/** Seed the chat store's session list (patchSessionPhenotype stays from the real store). */
function seedSessions(sessions: Session[]) {
  useChatStore.setState({ sessions });
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
  useChatStore.setState({ sessions: [] });
});

describe("PhenoSelector (#245 2b, per-session #935)", () => {
  it("shows the global active phenotype for a session with no binding", () => {
    seed({ activeId: "orchestrator" });
    seedSessions([session("s1")]);
    render(<PhenoSelector sessionId="s1" />);
    expect(screen.getByLabelText("Session phenotype").textContent).toContain(
      "Orchestrator",
    );
  });

  it("shows the session's own binding, independent of the global active", () => {
    // Global active is Orchestrator, but this session is bound to Rust.
    seed({ activeId: "orchestrator" });
    seedSessions([session("s1", "rust")]);
    render(<PhenoSelector sessionId="s1" />);
    expect(screen.getByLabelText("Session phenotype").textContent).toContain(
      "Rust",
    );
  });

  it("falls back to a placeholder when no phenotype is active yet", () => {
    seed({ profiles: [], activeId: "orchestrator", loading: true });
    seedSessions([session("s1")]);
    render(<PhenoSelector sessionId="s1" />);
    expect(screen.getByLabelText("Session phenotype").textContent).toContain(
      "Phenotype",
    );
  });

  it("hides the built-in default from the dropdown", async () => {
    seed({ activeId: "orchestrator" });
    seedSessions([session("s1")]);
    const user = userEvent.setup();
    render(<PhenoSelector sessionId="s1" />);

    await user.click(screen.getByLabelText("Session phenotype"));
    await screen.findByRole("menuitem", { name: /Rust/ });
    expect(screen.queryByRole("menuitem", { name: /^Default$/ })).toBeNull();
  });

  it("switching binds the phenotype to this session, not globally", async () => {
    const setSpy = vi.spyOn(ipc, "setSessionPhenotype").mockResolvedValue();
    const switchSpy = vi.spyOn(ipc, "switchPhenotype");
    seed({ activeId: "orchestrator" });
    seedSessions([session("s1")]);
    const user = userEvent.setup();
    render(<PhenoSelector sessionId="s1" />);

    await user.click(screen.getByLabelText("Session phenotype"));
    const rustItem = await screen.findByRole("menuitem", { name: /Rust/ });
    await user.click(rustItem);

    expect(setSpy).toHaveBeenCalledWith("s1", "rust");
    expect(switchSpy).not.toHaveBeenCalled();
    // Optimistic patch reflects immediately on this session.
    await waitFor(() =>
      expect(
        useChatStore.getState().sessions.find((s) => s.id === "s1")?.phenotype,
      ).toBe("rust"),
    );
  });

  it("reverts the optimistic binding when the switch rejects", async () => {
    vi.spyOn(ipc, "setSessionPhenotype").mockRejectedValue(new Error("nope"));
    seed({ activeId: "orchestrator" });
    seedSessions([session("s1", "orchestrator")]);
    const user = userEvent.setup();
    render(<PhenoSelector sessionId="s1" />);

    await user.click(screen.getByLabelText("Session phenotype"));
    await user.click(await screen.findByRole("menuitem", { name: /Rust/ }));

    await waitFor(() =>
      expect(
        useChatStore.getState().sessions.find((s) => s.id === "s1")?.phenotype,
      ).toBe("orchestrator"),
    );
  });

  it("renders a distinct error state when the phenotype load fails", async () => {
    vi.spyOn(ipc, "listPhenotypes").mockRejectedValue(new Error("boom"));
    useProfilesStore.setState({
      profiles: [],
      activeId: "orchestrator",
      loading: false,
      saving: false,
      error: null,
    });
    seedSessions([session("s1")]);
    const user = userEvent.setup();
    render(<PhenoSelector sessionId="s1" />);

    // The lazy auto-load fails and records the error (and must not retry-loop).
    await waitFor(() => expect(useProfilesStore.getState().error).toBe("boom"));

    await user.click(screen.getByLabelText("Session phenotype"));
    const item = await screen.findByRole("menuitem", {
      name: /Failed to load/i,
    });
    expect(item).toBeTruthy();
    expect(item.getAttribute("title")).toBe("boom");
  });

  it("documents the per-pane scope in the menu footer", async () => {
    seed();
    seedSessions([session("s1")]);
    const user = userEvent.setup();
    render(<PhenoSelector sessionId="s1" />);

    await user.click(screen.getByLabelText("Session phenotype"));
    expect(await screen.findByText(/Applies to this pane/i)).toBeTruthy();
  });
});
