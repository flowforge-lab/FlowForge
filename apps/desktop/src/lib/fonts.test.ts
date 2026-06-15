// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";

import { FONTS, applyFont, fontCssValue } from "./fonts";

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
