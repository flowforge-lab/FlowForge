// @vitest-environment jsdom

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import { ModelChip } from "@/components/model-chip";
import { ipc } from "@/lib/ipc";
import { useModelConfigStore } from "@/store/model-config";
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

describe("ModelChip inline Thinking toggle (#633)", () => {
  // Keyboard-operable menu checkbox (not a nested Switch): role menuitemcheckbox,
  // named via aria-label; the visible Switch is a presentational, aria-hidden mirror.
  const thinkingToggle = () =>
    screen.queryByRole("menuitemcheckbox", { name: "Thinking" });

  it("shows the Thinking toggle for a local resolved model (candle-vLLM default), reflecting its state", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL); // inherited → candle-vLLM (local)
    await userEvent.click(screen.getByLabelText("Session model"));

    const toggle = await screen.findByRole("menuitemcheckbox", {
      name: "Thinking",
    });
    // The mock candle-vLLM connection defaults thinking off for local kinds (#633).
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText(/off is faster on local models/i)).not.toBeNull();
  });

  it("also shows for an Ollama (local) override", async () => {
    await ipc.setSessionModelSelection("s2", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(<ModelChip sessionId="s2" />);
    await screen.findByText("qwen2.5");
    await userEvent.click(screen.getByLabelText("Session model"));
    expect(
      await screen.findByRole("menuitemcheckbox", { name: "Thinking" }),
    ).not.toBeNull();
  });

  it("hides the toggle for a hosted resolved model (OpenAI)", async () => {
    await ipc.setSessionModelSelection("s2", {
      connection: "openai",
      model: "gpt-4o",
    });
    render(<ModelChip sessionId="s2" />);
    await screen.findByText("gpt-4o");
    await userEvent.click(screen.getByLabelText("Session model"));
    // Dropdown is open (clear item present) but no reasoning toggle for hosted kinds.
    await screen.findByText("Use phenotype / global default");
    expect(thinkingToggle()).toBeNull();
  });

  it("toggling persists via upsertConnection and keeps the menu open", async () => {
    const upsert = vi
      .spyOn(ipc, "upsertConnection")
      .mockImplementation(async (conn) => conn);
    await ipc.setSessionModelSelection("s2", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(<ModelChip sessionId="s2" />);
    await screen.findByText("qwen2.5");
    await userEvent.click(screen.getByLabelText("Session model"));

    await userEvent.click(
      await screen.findByRole("menuitemcheckbox", { name: "Thinking" }),
    );

    // Ollama's mock `thinking` defaults off for local kinds (#633), so the toggle flips it on.
    expect(upsert).toHaveBeenCalledWith(
      expect.objectContaining({ id: "ollama", thinking: true }),
    );
    // Menu stayed open (onSelect preventDefault): the picker is still mounted.
    expect(screen.getByText("Use phenotype / global default")).not.toBeNull();
  });

  it("toggles via keyboard (Enter on the focused row) without a pointer (#640)", async () => {
    const upsert = vi
      .spyOn(ipc, "upsertConnection")
      .mockImplementation(async (conn) => conn);
    await ipc.setSessionModelSelection("s2", {
      connection: "ollama",
      model: "qwen2.5",
    });
    render(<ModelChip sessionId="s2" />);
    await screen.findByText("qwen2.5");
    await userEvent.click(screen.getByLabelText("Session model"));

    const toggle = await screen.findByRole("menuitemcheckbox", {
      name: "Thinking",
    });
    toggle.focus();
    await userEvent.keyboard("{Enter}");

    expect(upsert).toHaveBeenCalledWith(
      expect.objectContaining({ id: "ollama", thinking: true }),
    );
    expect(screen.getByText("Use phenotype / global default")).not.toBeNull();
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

describe("ModelChip per-provider model search (#1301)", () => {
  beforeEach(async () => {
    // The mock's SiliconFlow connection is keyless by default, and `listModels`
    // returns nothing for a hosted kind without a key (mirroring the real 401).
    // Store one so its ~32-model catalog loads — a catalog that size is the
    // whole reason this feature exists.
    await ipc.setProviderSecret("siliconflow", "apiKey", "sk-test");
    // A *cached empty list* counts as "already fetched", so a keyless fetch
    // from an earlier test would otherwise stick for the rest of the file.
    useModelConfigStore.setState({ modelsById: {} });
  });

  /** Open the chip, then the given provider's submenu. */
  const openProvider = async (name: string) => {
    await userEvent.click(screen.getByLabelText("Session model"));
    await userEvent.click(await screen.findByText(name));
  };
  const optionNames = () =>
    screen.queryAllByRole("option").map((o) => o.textContent);

  it("filters within the provider the user opened", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = await screen.findByLabelText("Search models");
    await waitFor(() => expect(optionNames().length).toBeGreaterThan(20));
    const total = optionNames().length;

    await userEvent.type(input, "qwen3coder");
    const shown = optionNames();
    // Subsequence matching can still admit a stray id; what the user needs is
    // that the list collapses and the family they typed ranks first.
    expect(shown.length).toBeLessThan(total / 3);
    expect(shown[0]).toMatch(/Qwen3-Coder/);
    // And the count says what was hidden.
    expect(screen.getByText(`${shown.length} of ${total}`)).not.toBeNull();
  });

  it("never reaches into another provider's catalog", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = await screen.findByLabelText("Search models");
    // `llama3.2` is an Ollama model; SiliconFlow's own Llama ids are named
    // differently, so this must come back empty rather than borrowing rows
    // from the neighbouring connection.
    await userEvent.type(input, "llama3.2");
    expect(await screen.findByText(/No models match/)).not.toBeNull();
  });

  it("picks the highlighted model with the keyboard alone", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = await screen.findByLabelText("Search models");
    await userEvent.type(input, "qwen3coder480");
    await waitFor(() =>
      expect(optionNames()).toEqual(["Qwen/Qwen3-Coder-480B-A35B-Instruct"]),
    );
    await userEvent.keyboard("{Enter}");

    // Assert on the chip itself: the model id also appears in the row that was
    // just clicked, so a bare text query is ambiguous.
    await waitFor(() =>
      expect(screen.getByLabelText("Session model").textContent).toContain(
        "Qwen/Qwen3-Coder-480B-A35B-Instruct",
      ),
    );
  });

  it("moves the highlight with the arrow keys instead of the menu's focus", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = await screen.findByLabelText("Search models");
    await userEvent.type(input, "qwen");
    await waitFor(() => expect(optionNames().length).toBeGreaterThan(1));

    const highlighted = () =>
      screen
        .queryAllByRole("option")
        .find((o) => o.getAttribute("aria-selected") === "true")?.textContent;
    const first = highlighted();
    await userEvent.keyboard("{ArrowDown}");

    expect(highlighted()).not.toBe(first);
    // Focus stayed in the box, so the next keystroke is still a search — this
    // is what a Radix menu's roving focus would otherwise break.
    expect(document.activeElement).toBe(input);
  });

  it("Escape clears the query before it closes the menu", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = (await screen.findByLabelText(
      "Search models",
    )) as HTMLInputElement;
    await userEvent.type(input, "qwen");
    expect(input.value).toBe("qwen");

    await userEvent.keyboard("{Escape}");
    expect(input.value).toBe("");
    expect(screen.getByLabelText("Search models")).not.toBeNull();
  });

  it("omits the box for a provider with only a handful of models", async () => {
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("Ollama"); // three models in the mock

    await screen.findByText("qwen2.5");
    expect(screen.queryByLabelText("Search models")).toBeNull();
  });

  it("still selects with the keyboard in a small provider's list", async () => {
    // #1302 review: the searchable list trades Radix's menu keyboard handling
    // for a search-box-driven one. A provider under the threshold has no box,
    // so it keeps real menu items — otherwise the majority of providers (most
    // list fewer than eight models) would become pointer-only.
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await userEvent.click(screen.getByLabelText("Session model"));

    // The keyboard route into a submenu: focus the provider, ArrowRight opens
    // it and lands on the first model, ArrowDown takes the second.
    const provider = await screen.findByText("Ollama");
    (provider.closest('[role="menuitem"]') as HTMLElement).focus();
    await userEvent.keyboard("{ArrowRight}");
    await screen.findByText("llama3.2");
    await userEvent.keyboard("{ArrowDown}{Enter}");

    await waitFor(() =>
      expect(screen.getByLabelText("Session model").textContent).toContain(
        "qwen2.5",
      ),
    );
  });

  it("announces the highlighted row to assistive tech", async () => {
    // The rows are plain options driven from the box, so the active one has to
    // be wired back to it or arrow navigation is silent (#1302 review).
    render(<ModelChip sessionId="s1" />);
    await screen.findByText(DEFAULT_MODEL);
    await openProvider("SiliconFlow");

    const input = await screen.findByLabelText("Search models");
    await waitFor(() => expect(optionNames().length).toBeGreaterThan(0));

    // Scope to the listbox this input controls: another provider's list can
    // still be mounted, and the pointer here must resolve within *this* one.
    const listId = input.getAttribute("aria-controls");
    expect(listId).toBeTruthy();
    const list = document.getElementById(listId as string);
    expect(list?.getAttribute("role")).toBe("listbox");

    const activeId = () => input.getAttribute("aria-activedescendant");
    const highlightedId = () =>
      [...(list?.querySelectorAll('[role="option"]') ?? [])].find(
        (o) => o.getAttribute("aria-selected") === "true",
      )?.id;

    expect(activeId()).toBe(highlightedId());

    await userEvent.keyboard("{ArrowDown}");
    expect(activeId()).toBe(highlightedId());
  });
});
