// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Badge } from "@/components/ui/badge";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => root.render(ui));
}

const badge = () => container.querySelector('[data-slot="badge"]');

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

describe("Badge (#284)", () => {
  it("renders its content and defaults to the neutral tone", () => {
    render(<Badge>draft</Badge>);
    expect(badge()?.textContent).toBe("draft");
    expect(badge()?.className).toContain("text-muted-foreground");
  });

  it("applies the requested tone (all production-used tones)", () => {
    render(<Badge tone="emerald">always</Badge>);
    expect(badge()?.className).toContain("text-emerald-700");
    render(<Badge tone="destructive">dangerous</Badge>);
    expect(badge()?.className).toContain("text-destructive");
    render(<Badge tone="amber">write</Badge>);
    expect(badge()?.className).toContain("text-amber-700");
    render(<Badge tone="sky">asking</Badge>);
    expect(badge()?.className).toContain("text-sky-700");
  });

  it("merges a passed className", () => {
    render(<Badge className="ml-2">x</Badge>);
    expect(badge()?.className).toContain("ml-2");
  });

  it("resolves twMerge conflicts so a passed class wins over the base", () => {
    // The neutral base sets `bg-muted`; a passed `bg-*` must override it (the
    // whole reason `cn` runs through twMerge), not duplicate.
    render(<Badge className="bg-red-500">x</Badge>);
    expect(badge()?.className).toContain("bg-red-500");
    expect(badge()?.className).not.toContain("bg-muted");
  });
});
