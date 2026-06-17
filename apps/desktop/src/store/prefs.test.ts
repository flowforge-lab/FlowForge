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

describe("appearance prefs (SET.2)", () => {
  beforeEach(() => {
    usePrefsStore.getState().resetAppearance();
  });

  it("defaults the new fields", () => {
    const s = usePrefsStore.getState();
    expect(s.fontScale).toBe(100);
    expect(s.displayName).toBe("");
    expect(s.openThreads).toBe(10);
    expect(s.notifications).toEqual({
      enabled: true,
      messageComplete: true,
      approvalRequests: true,
      sound: false,
    });
  });

  it("clamps fontScale and openThreads in their setters", () => {
    usePrefsStore.getState().setFontScale(9999);
    expect(usePrefsStore.getState().fontScale).toBe(140);
    usePrefsStore.getState().setFontScale(10);
    expect(usePrefsStore.getState().fontScale).toBe(80);

    usePrefsStore.getState().setOpenThreads(99);
    expect(usePrefsStore.getState().openThreads).toBe(20);
    usePrefsStore.getState().setOpenThreads(1);
    expect(usePrefsStore.getState().openThreads).toBe(3);
  });

  it("merges notification flags without dropping the others", () => {
    usePrefsStore.getState().setNotifications({ sound: true });
    expect(usePrefsStore.getState().notifications).toEqual({
      enabled: true,
      messageComplete: true,
      approvalRequests: true,
      sound: true,
    });
  });

  it("resetAppearance restores every appearance default", () => {
    usePrefsStore.setState({
      theme: "dark",
      font: "inter",
      fontScale: 130,
      displayName: "Ada",
      openThreads: 18,
      notifications: {
        enabled: false,
        messageComplete: false,
        approvalRequests: false,
        sound: true,
      },
    });
    usePrefsStore.getState().resetAppearance();
    const s = usePrefsStore.getState();
    expect(s).toMatchObject({
      theme: "system",
      font: "geist",
      fontScale: 100,
      displayName: "",
      openThreads: 10,
      notifications: {
        enabled: true,
        messageComplete: true,
        approvalRequests: true,
        sound: false,
      },
    });
  });
});

describe("keyboard prefs (SET.6)", () => {
  beforeEach(() => {
    usePrefsStore.getState().resetKeyboard();
  });

  it("defaults sendMessageKey to enter", () => {
    expect(usePrefsStore.getState().sendMessageKey).toBe("enter");
  });

  it("setSendMessageKey persists the binding", () => {
    usePrefsStore.getState().setSendMessageKey("ctrlEnter");
    expect(usePrefsStore.getState().sendMessageKey).toBe("ctrlEnter");
  });

  it("resetKeyboard restores enter without touching appearance prefs", () => {
    usePrefsStore.setState({ sendMessageKey: "ctrlEnter", theme: "dark" });
    usePrefsStore.getState().resetKeyboard();
    expect(usePrefsStore.getState().sendMessageKey).toBe("enter");
    // Appearance prefs are owned by resetAppearance, not resetKeyboard.
    expect(usePrefsStore.getState().theme).toBe("dark");
  });
});

describe("ff-prefs hydration of pre-SET.2 blobs", () => {
  afterEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  it("fills new keys with defaults when the persisted blob predates them", async () => {
    // A blob written before SET.2 — only theme/font present.
    localStorage.setItem(
      "ff-prefs",
      JSON.stringify({ state: { theme: "dark", font: "inter" }, version: 0 }),
    );
    vi.resetModules();
    const { usePrefsStore: freshStore } = await import("@/store/prefs");
    const s = freshStore.getState();
    // Preserved from the old blob…
    expect(s.theme).toBe("dark");
    expect(s.font).toBe("inter");
    // …and the new keys hydrate to defaults rather than undefined.
    expect(s.fontScale).toBe(100);
    expect(s.displayName).toBe("");
    expect(s.openThreads).toBe(10);
    expect(s.notifications.enabled).toBe(true);
  });
});
