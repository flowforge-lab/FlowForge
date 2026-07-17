import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Spy on the side-effect modules so we can assert the sound/title-flash gating
// without a DOM or an AudioContext.
vi.mock("@/lib/notification-sound", () => ({ playChime: vi.fn() }));
vi.mock("@/lib/title-flash", () => ({ flashTitle: vi.fn() }));

import { notify, allowedFor } from "@/lib/notify";
import { playChime } from "@/lib/notification-sound";
import { flashTitle } from "@/lib/title-flash";
import { usePrefsStore } from "@/store/prefs";
import { useSessionToastStore, type ToastKind } from "@/store/session-toast";
import type { NotificationPrefs } from "@/store/prefs";

const ALL_ON: NotificationPrefs = {
  enabled: true,
  messageComplete: true,
  approvalRequests: true,
  sound: false,
};

function setPrefs(partial: Partial<NotificationPrefs>) {
  usePrefsStore.setState({ notifications: { ...ALL_ON, ...partial } });
}

const kinds = () => useSessionToastStore.getState().toasts.map((t) => t.kind);

beforeEach(() => {
  useSessionToastStore.setState({ toasts: [] });
  setPrefs({});
  vi.clearAllMocks();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("allowedFor (gating map, #994)", () => {
  it("maps each kind to the right sub-toggle", () => {
    const on = ALL_ON;
    expect(allowedFor("done", { ...on, messageComplete: false })).toBe(false);
    expect(allowedFor("stopped", { ...on, messageComplete: false })).toBe(
      false,
    );
    expect(allowedFor("approval", { ...on, approvalRequests: false })).toBe(
      false,
    );
    // error is master-only — never gated by a sub-toggle.
    expect(
      allowedFor("error", {
        ...on,
        messageComplete: false,
        approvalRequests: false,
      }),
    ).toBe(true);
  });
});

describe("notify (#994)", () => {
  it("pushes a toast when the kind is allowed", () => {
    notify("done", "s1", "A");
    expect(kinds()).toEqual(["done"]);
  });

  it("master switch off silences every kind", () => {
    setPrefs({ enabled: false });
    (["done", "error", "approval", "stopped"] as ToastKind[]).forEach((k) =>
      notify(k, "s1", "A"),
    );
    expect(kinds()).toEqual([]);
  });

  it("'Message complete' off drops done + stopped but not error", () => {
    setPrefs({ messageComplete: false });
    notify("done", "s1", "A");
    notify("stopped", "s1", "A");
    notify("error", "s1", "A");
    expect(kinds()).toEqual(["error"]);
  });

  it("'Approval requests' off drops approval only", () => {
    setPrefs({ approvalRequests: false });
    notify("approval", "s1", "A");
    notify("done", "s1", "A");
    expect(kinds()).toEqual(["done"]);
  });

  it("plays the chime only when 'Sound' is on and the toast shows", () => {
    setPrefs({ sound: true });
    notify("done", "s1", "A");
    expect(playChime).toHaveBeenCalledTimes(1);

    vi.clearAllMocks();
    // Gated-out kind: no toast, no chime.
    setPrefs({ sound: true, messageComplete: false });
    notify("done", "s1", "A");
    expect(playChime).not.toHaveBeenCalled();
  });

  it("does not chime when 'Sound' is off", () => {
    notify("done", "s1", "A");
    expect(playChime).not.toHaveBeenCalled();
  });

  it("flashes the title for any shown toast, and not when suppressed", () => {
    notify("approval", "s1", "A");
    expect(flashTitle).toHaveBeenCalledWith("approval");

    vi.clearAllMocks();
    setPrefs({ approvalRequests: false });
    notify("approval", "s1", "A");
    expect(flashTitle).not.toHaveBeenCalled();
  });
});
