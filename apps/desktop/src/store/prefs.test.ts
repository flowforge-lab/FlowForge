// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { migrateLegacyTheme, usePrefsStore } from "@/store/prefs";

describe("migrateLegacyTheme", () => {
  afterEach(() => {
    localStorage.clear();
  });

  it("migrates ff-theme light/dark and removes the legacy key", () => {
    localStorage.setItem("ff-theme", "light");
    expect(migrateLegacyTheme()).toEqual({ theme: "light" });
    expect(localStorage.getItem("ff-theme")).toBeNull();

    localStorage.setItem("ff-theme", "dark");
    expect(migrateLegacyTheme()).toEqual({ theme: "dark" });
    expect(localStorage.getItem("ff-theme")).toBeNull();
  });

  it("returns {} when no legacy key is present", () => {
    expect(migrateLegacyTheme()).toEqual({});
  });
});

describe("usePrefsStore toggleTheme", () => {
  beforeEach(() => {
    usePrefsStore.setState({ theme: "system", font: "geist" });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('from "system" sets explicit light when OS effective mode is dark', () => {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: (query: string) => ({
        matches: query.includes("dark"),
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    });
    usePrefsStore.getState().toggleTheme();
    expect(usePrefsStore.getState().theme).toBe("light");
  });

  it('from "system" sets explicit dark when OS effective mode is light', () => {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: () => ({
        matches: false,
        media: "",
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    });
    usePrefsStore.getState().toggleTheme();
    expect(usePrefsStore.getState().theme).toBe("dark");
  });

  it("flips between light and dark when already explicit", () => {
    usePrefsStore.setState({ theme: "light" });
    usePrefsStore.getState().toggleTheme();
    expect(usePrefsStore.getState().theme).toBe("dark");
    usePrefsStore.getState().toggleTheme();
    expect(usePrefsStore.getState().theme).toBe("light");
  });
});
