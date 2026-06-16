import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SettingsShell } from "@/components/settings/settings-shell";
import { SettingsSectionContent } from "@/components/settings/section";
import { SETTINGS_NAV } from "@/components/settings/registry";
import { useSettingsStore } from "@/store/settings";

const ALL_LABELS = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.label));

describe("SettingsShell", () => {
  it("mounts with the two-group nav listing all 10 sections", () => {
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

  it("shows the migrated theme + web-search UI in the default Appearance section", () => {
    const html = renderToStaticMarkup(<SettingsShell />);
    expect(html).toContain("Font");
    expect(html).toContain("Web search");
    expect(html).not.toContain("Coming soon.");
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

  it("routes appearance to the migrated UI and unbuilt sections to ComingSoon", () => {
    const appearance = renderToStaticMarkup(
      <SettingsSectionContent id="appearance" />,
    );
    expect(appearance).toContain("Web search");
    expect(appearance).not.toContain("Coming soon.");

    const model = renderToStaticMarkup(<SettingsSectionContent id="model" />);
    expect(model).toContain("Coming soon.");
    expect(model).toContain("Model");
    expect(model).not.toContain("Web search");
  });
});
