// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { InstalledTab } from "@/components/settings/profiles/installed-tab";
import {
  phenotypeToProfile,
  useProfilesStore,
  type Profile,
} from "@/store/profiles";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => {
    root.render(ui);
  });
}

/** The card button whose name matches `name`. */
function card(name: string): HTMLElement | null {
  return (
    [...container.querySelectorAll<HTMLElement>("button[aria-pressed]")].find(
      (b) => b.textContent?.includes(name),
    ) ?? null
  );
}

/** Does the named card show the ⭐ "Default profile" star? */
function hasStar(name: string): boolean {
  return !!card(name)?.querySelector('[aria-label="Default profile"]');
}

function seed(profiles: Profile[]) {
  useProfilesStore.setState({
    profiles,
    activeId: "default",
    loading: false,
    saving: false,
    error: null,
  });
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("InstalledTab default star", () => {
  it("stars Codon when it is installed (and not the built-in default)", () => {
    seed([
      phenotypeToProfile({ name: "default", skills: [], mcpServers: [] }, 0),
      phenotypeToProfile(
        { name: "codon", skills: ["codegraph"], mcpServers: [] },
        1,
      ),
    ]);
    render(<InstalledTab />);
    expect(hasStar("Codon")).toBe(true);
    expect(hasStar("Default")).toBe(false);
  });

  it("stars the built-in default when Codon is absent", () => {
    seed([
      phenotypeToProfile({ name: "default", skills: [], mcpServers: [] }, 0),
      phenotypeToProfile({ name: "rust", skills: [], mcpServers: [] }, 1),
    ]);
    render(<InstalledTab />);
    expect(hasStar("Default")).toBe(true);
    expect(hasStar("Rust")).toBe(false);
  });
});
