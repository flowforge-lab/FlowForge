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
  it("lists geist and inter with css values", () => {
    expect(FONTS.map((f) => f.id)).toEqual(["geist", "inter"]);
    expect(fontCssValue("geist")).toContain("Geist");
    expect(fontCssValue("inter")).toContain("Inter");
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
