// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";

import { THEMES, applyTheme, resolveEffectiveTheme } from "./theme";

describe("theme registry", () => {
  it("exports system, light, and dark entries with labels and swatches", () => {
    expect(THEMES.map((t) => t.id)).toEqual(["system", "light", "dark"]);
    for (const t of THEMES) {
      expect(t.label.length).toBeGreaterThan(0);
      expect(t.previewBg.length).toBeGreaterThan(0);
    }
  });
});

describe("resolveEffectiveTheme", () => {
  afterEach(() => {
    document.documentElement.classList.remove("dark");
  });

  it('returns "light" or "dark" for system based on matchMedia', () => {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: (query: string) => ({
        matches: query.includes("dark"),
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    });
    const result = resolveEffectiveTheme("system");
    expect(result === "light" || result === "dark").toBe(true);
  });

  it("returns fixed modes unchanged", () => {
    expect(resolveEffectiveTheme("light")).toBe("light");
    expect(resolveEffectiveTheme("dark")).toBe("dark");
  });
});

describe("applyTheme", () => {
  afterEach(() => {
    document.documentElement.classList.remove("dark");
  });

  it("adds .dark for dark preference", () => {
    applyTheme("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("removes .dark for light preference", () => {
    document.documentElement.classList.add("dark");
    applyTheme("light");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });
});
