import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SettingsShell } from "@/components/settings/settings-shell";
import { SettingsSectionContent } from "@/components/settings/section";
import { AppearanceSection } from "@/components/settings/appearance-section";
import { AboutSection } from "@/components/settings/about-section";
import { MemorySection } from "@/components/settings/memory-section";
import { SETTINGS_NAV } from "@/components/settings/registry";
import { useSettingsStore } from "@/store/settings";

const ALL_LABELS = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.label));

describe("SettingsShell", () => {
  it("mounts with the two-group nav listing all sections", () => {
    const html = renderToStaticMarkup(<SettingsShell />);
    expect(html).toContain("PROFILE");
    expect(html).toContain("GLOBAL");
    for (const label of ALL_LABELS) {
      expect(html).toContain(label);
    }
  });

  it("renders a disabled Reset to defaults footer button when no section wires it", () => {
    const html = renderToStaticMarkup(<SettingsShell />);
    expect(html).toContain("Reset to defaults");
    // Disabled until the active section registers a handler.
    expect(html).toContain("disabled");
  });
});

// #599 item 6: section bodies are lazy-loaded (`React.lazy` + `Suspense`) so each
// section is a separate chunk kept out of the initial bundle (proof: the build
// emits per-section chunks; the main entry shrinks). The section components
// themselves are unchanged — routing/content is asserted directly below via each
// section component, which is stable regardless of lazy-resolution timing. (We
// avoid asserting lazy *resolution* through `renderToStaticMarkup`, which resolves
// lazy boundaries non-deterministically depending on module-cache warmth.)
describe("SettingsSectionContent", () => {
  it("renders without throwing for every section id (Suspense boundary)", () => {
    for (const id of [
      "appearance",
      "model",
      "skills",
      "profiles",
      "control",
      "mcp",
      "memory",
      "keyboard",
      "scheduled",
      "experimental",
      "about",
    ] as const) {
      expect(() =>
        renderToStaticMarkup(<SettingsSectionContent id={id} />),
      ).not.toThrow();
    }
  });
});

// The section components are unchanged by the code-split; assert their content
// directly (previously routed synchronously through SettingsSectionContent).
describe("section content", () => {
  it("appearance renders its sub-tabs (Theme active by default)", () => {
    const html = renderToStaticMarkup(<AppearanceSection />);
    expect(html).toContain("Mode");
    expect(html).toContain("Font");
    expect(html).toContain("Notifications");
    expect(html).toContain("Advanced");
    expect(html).not.toContain("Coming soon.");
  });

  it("about renders its actions", () => {
    const html = renderToStaticMarkup(<AboutSection />);
    expect(html).toContain("Check for updates");
    expect(html).toContain("View all keyboard shortcuts");
    expect(html).not.toContain("Coming soon.");
  });

  it("memory renders its category cards (SET.8)", () => {
    const html = renderToStaticMarkup(<MemorySection />);
    expect(html).not.toContain("Coming soon.");
    expect(html).toContain("Identity");
    expect(html).toContain("Patterns");
    expect(html).toContain("Focus");
  });
});

// `setSection` switches the active pane: the store flips `activeSection` and the
// content router maps it to the right body. (renderToStaticMarkup can't observe
// store mutations — zustand's server snapshot is the initial state — so the two
// halves are exercised directly.)
describe("section switching", () => {
  it("setSection updates the active section and clears the prior reset handler", () => {
    useSettingsStore.setState({
      activeSection: "appearance",
      resetHandler: () => {},
    });
    useSettingsStore.getState().setSection("model");
    expect(useSettingsStore.getState().activeSection).toBe("model");
    expect(useSettingsStore.getState().resetHandler).toBeNull();
  });
});
