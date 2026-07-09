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

import { PhenotypeDetail } from "@/components/settings/profiles/phenotype-detail";
import { InstalledTab } from "@/components/settings/profiles/installed-tab";
import { ipc } from "@/lib/ipc";
import { useModelConfigStore } from "@/store/model-config";
import { useProfilesStore } from "@/store/profiles";

// Radix DropdownMenu calls pointer/scroll APIs jsdom lacks.
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
});

// Hydrate both stores from the shared mock so the editor sees real connections +
// phenotypes (reviewer is bound to openai/gpt-4o in the mock — exercises a set value).
beforeEach(async () => {
  await ipc.switchPhenotype("default");
  await useModelConfigStore.getState().load();
  await useProfilesStore.getState().load();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("PhenotypeDetail (#530)", () => {
  it("renders the built-in default read-only with a working Duplicate action", async () => {
    // Mock (no call-through) so the shared mock's phenotype set isn't mutated.
    const spy = vi.spyOn(ipc, "updatePhenotype").mockResolvedValue({
      name: "default-copy",
      skills: [],
      mcpServers: [],
      egress: "open",
    });
    render(<PhenotypeDetail phenotypeId="default" />);

    // No provider/model pickers — the rows are read-only.
    expect(screen.queryByText("OpenAI")).toBeNull();
    const dup = screen.getByRole("button", { name: /Duplicate to customize/i });
    await userEvent.click(dup);

    await waitFor(() => expect(spy).toHaveBeenCalled());
    expect(spy.mock.calls[0][0]).toMatchObject({ name: "default-copy" });
  });

  it("shows the phenotype's bound provider and writes the whole record on change", async () => {
    const spy = vi.spyOn(ipc, "updatePhenotype").mockResolvedValue({
      name: "reviewer",
      skills: ["create-pr"],
      mcpServers: [],
      egress: "open",
    });
    render(<PhenotypeDetail phenotypeId="reviewer" />);

    // The reviewer phenotype is bound to OpenAI in the mock.
    await screen.findByRole("button", { name: /OpenAI/ });
    await userEvent.click(screen.getByRole("button", { name: /OpenAI/ }));
    await userEvent.click(
      await screen.findByRole("menuitem", { name: /Ollama/ }),
    );

    await waitFor(() => expect(spy).toHaveBeenCalled());
    // Lossless: the whole phenotype is written, only `provider` changed.
    expect(spy.mock.calls[0][0]).toMatchObject({
      name: "reviewer",
      provider: "ollama",
      model: "gpt-4o",
      skills: ["create-pr"],
    });
  });
});

describe("InstalledTab → editor (#530)", () => {
  it("selecting a card opens its detail/editor panel", async () => {
    render(<InstalledTab />);
    // Clicking a card selects it; the reviewer panel surfaces its bound provider.
    const card = await screen.findByRole("button", { name: /Reviewer/ });
    await userEvent.click(card);
    expect(
      await screen.findByRole("button", { name: /OpenAI/ }),
    ).not.toBeNull();
  });
});
