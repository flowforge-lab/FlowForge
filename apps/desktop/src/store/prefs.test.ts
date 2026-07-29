// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  BOOT_PREFS_KEY,
  clampSidebarWidth,
  initPrefs,
  migrateLegacyTheme,
  mirrorBootPrefs,
  usePrefsStore,
  SIDEBAR_WIDTH_DEFAULT,
  SIDEBAR_WIDTH_MIN,
  SIDEBAR_WIDTH_MAX,
} from "@/store/prefs";
import { ipc } from "@/lib/ipc";

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

  it("defaults defaultMode to auto and setDefaultMode updates it (#266)", () => {
    expect(usePrefsStore.getState().defaultMode).toBe("auto");
    usePrefsStore.getState().setDefaultMode("plan");
    expect(usePrefsStore.getState().defaultMode).toBe("plan");
  });

  it("does NOT persist defaultMode to ff-prefs (#287 — backend is the source of truth)", () => {
    usePrefsStore.getState().setDefaultMode("plan");
    const blob = JSON.parse(localStorage.getItem("ff-prefs") ?? "{}");
    expect(blob.state?.defaultMode).toBeUndefined();
    // A persisted pref (sendMessageKey) still round-trips, proving persistence runs.
    usePrefsStore.getState().setSendMessageKey("ctrlEnter");
    expect(
      JSON.parse(localStorage.getItem("ff-prefs") ?? "{}").state.sendMessageKey,
    ).toBe("ctrlEnter");
  });

  it("resetKeyboard restores defaultMode to auto (#266)", () => {
    usePrefsStore.setState({ defaultMode: "plan" });
    usePrefsStore.getState().resetKeyboard();
    expect(usePrefsStore.getState().defaultMode).toBe("auto");
  });
});

describe("default mode ↔ backend mode.json (#798)", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    usePrefsStore.setState({ defaultMode: "auto" });
  });

  it("setDefaultMode writes through to set_default_mode", () => {
    const spy = vi.spyOn(ipc, "setDefaultMode").mockResolvedValue();
    usePrefsStore.getState().setDefaultMode("plan");
    // Store is authoritative for the UI…
    expect(usePrefsStore.getState().defaultMode).toBe("plan");
    // …and mirrored to the backend so it survives a relaunch.
    expect(spy).toHaveBeenCalledWith("plan");
  });

  it("hydrateDefaultMode pulls the persisted value from get_default_mode", async () => {
    vi.spyOn(ipc, "getDefaultMode").mockResolvedValue("act");
    await usePrefsStore.getState().hydrateDefaultMode();
    expect(usePrefsStore.getState().defaultMode).toBe("act");
  });

  it("hydrateDefaultMode keeps the current default when the backend rejects", async () => {
    usePrefsStore.setState({ defaultMode: "plan" });
    vi.spyOn(ipc, "getDefaultMode").mockRejectedValue(new Error("down"));
    await usePrefsStore.getState().hydrateDefaultMode();
    expect(usePrefsStore.getState().defaultMode).toBe("plan");
  });

  it("resetKeyboard writes the default-mode reset through to the backend", () => {
    const spy = vi.spyOn(ipc, "setDefaultMode").mockResolvedValue();
    usePrefsStore.setState({ defaultMode: "plan" });
    usePrefsStore.getState().resetKeyboard();
    expect(usePrefsStore.getState().defaultMode).toBe("auto");
    expect(spy).toHaveBeenCalledWith("auto");
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
    expect(s.sidebarCollapsed).toBe(false);
  });
});

describe("sidebar collapse preference (#185)", () => {
  afterEach(() => {
    localStorage.clear();
  });

  it("persists sidebarCollapsed in ff-prefs", () => {
    usePrefsStore.getState().setSidebarCollapsed(true);
    expect(
      JSON.parse(localStorage.getItem("ff-prefs") ?? "{}").state
        .sidebarCollapsed,
    ).toBe(true);
  });
});

describe("sidebar width preference (#204)", () => {
  afterEach(() => {
    localStorage.clear();
    usePrefsStore.getState().setSidebarWidth(SIDEBAR_WIDTH_DEFAULT);
  });

  it("defaults to SIDEBAR_WIDTH_DEFAULT", () => {
    expect(usePrefsStore.getState().sidebarWidth).toBe(SIDEBAR_WIDTH_DEFAULT);
  });

  it("clamps to the usable bounds and rounds", () => {
    expect(clampSidebarWidth(10)).toBe(SIDEBAR_WIDTH_MIN);
    expect(clampSidebarWidth(9999)).toBe(SIDEBAR_WIDTH_MAX);
    expect(clampSidebarWidth(300.6)).toBe(301);
  });

  it("setSidebarWidth clamps and persists in ff-prefs", () => {
    usePrefsStore.getState().setSidebarWidth(10_000);
    expect(usePrefsStore.getState().sidebarWidth).toBe(SIDEBAR_WIDTH_MAX);
    expect(
      JSON.parse(localStorage.getItem("ff-prefs") ?? "{}").state.sidebarWidth,
    ).toBe(SIDEBAR_WIDTH_MAX);
  });
});

// Prefs moved off plain localStorage in #1121, but the pre-paint script in
// index.html can only read something synchronous — so the store keeps a
// theme+font copy in localStorage purely for that script. These tests pin the
// contract that script depends on: the key, and the shape it parses.
describe("pre-paint boot mirror (#1121)", () => {
  afterEach(() => {
    localStorage.clear();
  });

  it("mirrors theme and font under BOOT_PREFS_KEY in the shape index.html parses", () => {
    mirrorBootPrefs("dark", "nunito");
    const raw = localStorage.getItem(BOOT_PREFS_KEY);
    expect(raw).not.toBeNull();
    // The boot script does `parsed.state || parsed`, then reads .theme/.font.
    const parsed = JSON.parse(raw ?? "{}");
    expect(parsed.state ?? parsed).toEqual({ theme: "dark", font: "nunito" });
  });

  it("is written under its own key, never the store's — durableStorage adopts and clears ff-prefs", () => {
    mirrorBootPrefs("light", "geist");
    expect(BOOT_PREFS_KEY).not.toBe("ff-prefs");
    expect(localStorage.getItem("ff-prefs")).toBeNull();
  });

  it("refreshes on every prefs change so the next launch paints the latest theme", () => {
    initPrefs();
    usePrefsStore.getState().setTheme("dark");
    expect(JSON.parse(localStorage.getItem(BOOT_PREFS_KEY) ?? "{}").theme).toBe(
      "dark",
    );

    usePrefsStore.getState().setTheme("light");
    expect(JSON.parse(localStorage.getItem(BOOT_PREFS_KEY) ?? "{}").theme).toBe(
      "light",
    );
  });
});

describe("async hydration signal (#1121)", () => {
  it("flips hasHydrated once the (async) read lands, and never persists it", async () => {
    localStorage.setItem(
      "ff-prefs",
      JSON.stringify({ state: { theme: "dark" }, version: 0 }),
    );
    vi.resetModules();
    const { usePrefsStore: fresh } = await import("@/store/prefs");

    await fresh.persist.rehydrate();
    expect(fresh.getState().hasHydrated).toBe(true);
    expect(fresh.getState().theme).toBe("dark");
    // Runtime-only: a persisted `true` must not survive into the next launch's
    // blob, or the gate it guards would open before the read has landed.
    expect(
      JSON.parse(localStorage.getItem("ff-prefs") ?? "{}").state.hasHydrated,
    ).toBeUndefined();
    localStorage.clear();
  });
});
