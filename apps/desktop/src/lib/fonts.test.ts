// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";

import {
  FONTS,
  FONT_SCALE_MAX,
  FONT_SCALE_MIN,
  applyFont,
  applyFontScale,
  clampFontScale,
  fontCssValue,
} from "./fonts";

describe("font registry", () => {
  it("lists all five typefaces with css values", () => {
    expect(FONTS.map((f) => f.id)).toEqual([
      "geist",
      "inter",
      "nunito",
      "manrope",
      "jetbrains-mono",
    ]);
    expect(fontCssValue("geist")).toContain("Geist");
    expect(fontCssValue("inter")).toContain("Inter");
    expect(fontCssValue("nunito")).toContain("Nunito");
    expect(fontCssValue("manrope")).toContain("Manrope");
    expect(fontCssValue("jetbrains-mono")).toContain("JetBrains Mono");
  });

  it("gives JetBrains Mono a monospace fallback", () => {
    expect(fontCssValue("jetbrains-mono")).toContain("monospace");
  });

  it("gives every font a non-empty label", () => {
    for (const f of FONTS) {
      expect(f.label.length).toBeGreaterThan(0);
    }
  });
});

describe("applyFont", () => {
  afterEach(() => {
    document.documentElement.style.removeProperty("--font-sans");
  });

  it("sets --font-sans on the document root", () => {
    applyFont("inter");
    expect(document.documentElement.style.getPropertyValue("--font-sans")).toBe(
      '"Inter Variable", sans-serif',
    );
  });

  it("restores geist when selected", () => {
    applyFont("inter");
    applyFont("geist");
    expect(document.documentElement.style.getPropertyValue("--font-sans")).toBe(
      '"Geist Variable", sans-serif',
    );
  });

  it("applies each lazy-loaded face's css value", () => {
    for (const id of ["nunito", "manrope", "jetbrains-mono"] as const) {
      applyFont(id);
      expect(
        document.documentElement.style.getPropertyValue("--font-sans"),
      ).toBe(fontCssValue(id));
    }
  });
});

describe("font scale", () => {
  afterEach(() => {
    document.documentElement.style.removeProperty("font-size");
  });

  it("clamps to the supported range", () => {
    expect(clampFontScale(40)).toBe(FONT_SCALE_MIN);
    expect(clampFontScale(999)).toBe(FONT_SCALE_MAX);
    expect(clampFontScale(112.4)).toBe(112);
  });

  it("applies the scale as a root font-size percentage", () => {
    applyFontScale(120);
    expect(document.documentElement.style.fontSize).toBe("120%");
  });

  it("clamps out-of-range scales before applying", () => {
    applyFontScale(1000);
    expect(document.documentElement.style.fontSize).toBe(`${FONT_SCALE_MAX}%`);
  });
});
