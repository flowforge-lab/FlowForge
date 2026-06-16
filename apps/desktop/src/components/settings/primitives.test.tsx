// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsSwitch } from "@/components/settings/switch";
import { SettingsSlider } from "@/components/settings/slider";
import { SegmentedControl } from "@/components/settings/segmented-control";
import { SubTabs } from "@/components/settings/sub-tabs";

// Minimal shims so radix primitives mount under jsdom (no layout engine).
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver ||= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
for (const m of [
  "hasPointerCapture",
  "setPointerCapture",
  "releasePointerCapture",
] as const) {
  // @ts-expect-error — patching jsdom prototypes
  Element.prototype[m] ||= () =>
    m === "hasPointerCapture" ? false : undefined;
}

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => {
    root.render(ui);
  });
}

function click(el: Element | null) {
  act(() => {
    el?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
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

describe("SettingsSwitch", () => {
  it("exposes role=switch with the label as accessible name", () => {
    render(
      <SettingsSwitch
        label="Notifications"
        checked={false}
        onCheckedChange={() => {}}
      />,
    );
    const sw = container.querySelector('[role="switch"]');
    expect(sw).not.toBeNull();
    expect(sw?.getAttribute("aria-label")).toBe("Notifications");
    expect(sw?.getAttribute("aria-checked")).toBe("false");
  });

  it("fires onCheckedChange on click", () => {
    const onCheckedChange = vi.fn();
    render(
      <SettingsSwitch
        label="Notifications"
        checked={false}
        onCheckedChange={onCheckedChange}
      />,
    );
    click(container.querySelector('[role="switch"]'));
    expect(onCheckedChange).toHaveBeenCalledWith(true);
  });

  it("does not fire when disabled", () => {
    const onCheckedChange = vi.fn();
    render(
      <SettingsSwitch
        label="Notifications"
        checked={false}
        disabled
        onCheckedChange={onCheckedChange}
      />,
    );
    const sw = container.querySelector('[role="switch"]');
    expect(sw).toHaveProperty("disabled", true);
    click(sw);
    expect(onCheckedChange).not.toHaveBeenCalled();
  });
});

describe("SettingsSlider", () => {
  it("exposes role=slider with label, value bounds, and a readout", () => {
    render(
      <SettingsSlider
        label="Temperature"
        value={30}
        min={0}
        max={100}
        onValueChange={() => {}}
        formatValue={(v) => `${v}%`}
      />,
    );
    const slider = container.querySelector('[role="slider"]');
    expect(slider?.getAttribute("aria-valuenow")).toBe("30");
    expect(slider?.getAttribute("aria-valuemin")).toBe("0");
    expect(slider?.getAttribute("aria-valuemax")).toBe("100");
    // Accessible name comes from the Root's aria-label.
    expect(
      container.querySelector('[aria-label="Temperature"]'),
    ).not.toBeNull();
    expect(container.textContent).toContain("30%");
  });

  it("steps the value with the arrow keys", () => {
    const onValueChange = vi.fn();
    render(
      <SettingsSlider
        label="Temperature"
        value={30}
        step={5}
        onValueChange={onValueChange}
      />,
    );
    const slider = container.querySelector('[role="slider"]') as HTMLElement;
    act(() => {
      slider.focus();
      slider.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
      );
    });
    expect(onValueChange).toHaveBeenCalledWith(35);
  });
});

describe("SegmentedControl", () => {
  const options = [
    { value: "low", label: "Low" },
    { value: "high", label: "High" },
  ] as const;

  it("renders a labeled group and marks the active option", () => {
    render(
      <SegmentedControl
        label="Effort"
        options={options}
        value="low"
        onValueChange={() => {}}
      />,
    );
    const group = container.querySelector('[aria-label="Effort"]');
    expect(group).not.toBeNull();
    const active = container.querySelector('[data-state="on"]');
    expect(active?.textContent).toBe("Low");
  });

  it("fires onValueChange with the clicked option", () => {
    const onValueChange = vi.fn();
    render(
      <SegmentedControl
        label="Effort"
        options={options}
        value="low"
        onValueChange={onValueChange}
      />,
    );
    const items = container.querySelectorAll("button");
    click(items[1]);
    expect(onValueChange).toHaveBeenCalledWith("high");
  });
});

describe("SubTabs", () => {
  const tabs = [
    { value: "theme", label: "Theme" },
    { value: "advanced", label: "Advanced" },
  ] as const;

  it("renders a tablist with the active tab selected", () => {
    render(
      <SubTabs
        label="Sections"
        tabs={tabs}
        value="theme"
        onValueChange={() => {}}
      />,
    );
    expect(container.querySelector('[role="tablist"]')).not.toBeNull();
    const selected = container.querySelector(
      '[role="tab"][aria-selected="true"]',
    );
    expect(selected?.textContent).toBe("Theme");
  });

  it("fires onValueChange when another tab is activated", () => {
    const onValueChange = vi.fn();
    render(
      <SubTabs
        label="Sections"
        tabs={tabs}
        value="theme"
        onValueChange={onValueChange}
      />,
    );
    const tabEls = container.querySelectorAll('[role="tab"]');
    // Radix Tabs uses automatic activation: focusing a trigger selects it.
    act(() => {
      (tabEls[1] as HTMLElement).focus();
    });
    expect(onValueChange).toHaveBeenCalledWith("advanced");
  });

  it("wires the active tab to its panel when given children", () => {
    render(
      <SubTabs
        label="Sections"
        tabs={tabs}
        value="theme"
        onValueChange={() => {}}
      >
        <p>Theme pane</p>
      </SubTabs>,
    );
    const panel = container.querySelector('[role="tabpanel"]');
    const activeTab = container.querySelector(
      '[role="tab"][aria-selected="true"]',
    );
    expect(panel?.textContent).toContain("Theme pane");
    // The active trigger's aria-controls resolves to the rendered panel, and the
    // panel is labelled by its trigger — the wiring a bar-only Tabs would forfeit.
    expect(activeTab?.getAttribute("aria-controls")).toBe(panel?.id);
    expect(panel?.getAttribute("aria-labelledby")).toBe(activeTab?.id);
  });
});
