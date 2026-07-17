// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  initTitleFlash,
  flashTitle,
  __resetTitleFlashForTest,
} from "@/lib/title-flash";

const BASE = "FlowForge";

// jsdom reports document.hasFocus() === true and visibilityState "visible" by
// default; override per-test to simulate a backgrounded window.
function setForeground(foreground: boolean) {
  vi.spyOn(document, "hasFocus").mockReturnValue(foreground);
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => (foreground ? "visible" : "hidden"),
  });
}

beforeEach(() => {
  __resetTitleFlashForTest();
  document.title = BASE;
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("title-flash (#994)", () => {
  it("no-ops while the window is in the foreground", () => {
    setForeground(true);
    initTitleFlash();
    flashTitle("approval");
    expect(document.title).toBe(BASE);
  });

  it("flashes the kind label while backgrounded", () => {
    setForeground(false);
    initTitleFlash();
    flashTitle("approval");
    expect(document.title).toBe(`● Needs approval — ${BASE}`);

    // A second flash of a different kind replaces the label, not stacks.
    flashTitle("error");
    expect(document.title).toBe(`● Failed — ${BASE}`);
  });

  it("restores the base title on focus", () => {
    setForeground(false);
    initTitleFlash();
    flashTitle("done");
    expect(document.title).not.toBe(BASE);

    setForeground(true);
    window.dispatchEvent(new Event("focus"));
    expect(document.title).toBe(BASE);
  });

  it("restores the base title on visibilitychange back to visible", () => {
    setForeground(false);
    initTitleFlash();
    flashTitle("stopped");
    expect(document.title).not.toBe(BASE);

    setForeground(true);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(document.title).toBe(BASE);
  });
});
