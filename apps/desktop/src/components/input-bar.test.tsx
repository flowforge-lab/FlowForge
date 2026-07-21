// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputBar } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { useCommandShortcutsStore } from "@/store/command-shortcuts";
import { usePrefsStore } from "@/store/prefs";
import { useSkillsStore } from "@/store/skills";
import type { Session, SkillInfo } from "@/bindings";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver ||= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
for (const m of [
  "hasPointerCapture",
  "setPointerCapture",
  "releasePointerCapture",
] as const) {
  // @ts-expect-error — patching jsdom prototypes
  Element.prototype[m] ||= () =>
    m === "hasPointerCapture" ? false : undefined;
}
globalThis.requestAnimationFrame ||= ((cb: FrameRequestCallback) =>
  setTimeout(
    () => cb(0),
    0,
  ) as unknown as number) as typeof requestAnimationFrame;

const SID = "s1";

let container: HTMLDivElement;
let root: Root;

function render() {
  act(() => {
    root.render(<InputBar sessionId={SID} />);
  });
}

function textarea(): HTMLTextAreaElement {
  const el = container.querySelector<HTMLTextAreaElement>("[data-composer]");
  if (!el) throw new Error("composer textarea not found");
  return el;
}

/** React tracks the DOM value, so go through the native setter to fire onChange. */
function type(text: string) {
  const el = textarea();
  act(() => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )?.set?.call(el, text);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** Returns the synthetic event so a test can assert `defaultPrevented`. */
function press(key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const ev = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    textarea().dispatchEvent(ev);
  });
  return ev;
}

function listbox(): HTMLElement | null {
  return container.querySelector(
    '[role="listbox"][aria-label="Slash commands"]',
  );
}

function rows(): HTMLElement[] {
  return [
    ...(listbox()?.querySelectorAll<HTMLElement>('[role="option"]') ?? []),
  ];
}

function activeRow(): HTMLElement | undefined {
  return rows().find((r) => r.getAttribute("aria-selected") === "true");
}

function composerText(): string {
  return useComposerStore.getState().textBySession[SID] ?? "";
}

const skill = (over: Partial<SkillInfo>): SkillInfo => ({
  name: "grill-me",
  description: "Adversarial review",
  version: "1.0.0",
  keywords: [],
  active: false,
  score: 0,
  ...over,
});

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });

  useChatStore.setState({
    activeSessionId: SID,
    // Only `id` and `phenotype` matter here (the composer reads the binding to
    // decide whether activated skills reach this session's turns).
    sessions: [{ id: SID, title: "Test" } as Session],
    streamingBySession: {},
    turnStartBySession: {},
  });
  useComposerStore.setState({
    textBySession: {},
    attachmentsBySession: {},
    editingBySession: {},
  });
  useSkillsStore.setState({
    skills: [skill({}), skill({ name: "tdd", description: "Test first" })],
  });
  useCommandShortcutsStore.setState({
    shortcuts: [{ id: "sc1", name: "ship", message: "Open a PR and push." }],
  });
  usePrefsStore.setState({ sendMessageKey: "enter" });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("InputBar — slash-command dropdown (#1036)", () => {
  it("stays closed for ordinary text", () => {
    render();
    type("hello there");
    expect(listbox()).toBeNull();
  });

  it("opens on a bare slash listing all three classes", () => {
    render();
    type("/");
    const labels = rows().map((r) => r.textContent ?? "");
    expect(labels.some((t) => t.includes("/goal"))).toBe(true);
    expect(labels.some((t) => t.includes("/grill-me"))).toBe(true);
    expect(labels.some((t) => t.includes("/ship"))).toBe(true);
  });

  it("narrows as the token is typed and closes on the following space", () => {
    render();
    type("/gr");
    expect(rows()[0].textContent).toContain("/grill-me");

    type("/goal ");
    expect(listbox()).toBeNull();
  });

  it("moves the highlight with the arrow keys, wrapping at the ends", () => {
    render();
    type("/");
    expect(activeRow()).toBe(rows()[0]);

    press("ArrowDown");
    expect(activeRow()).toBe(rows()[1]);

    press("ArrowUp");
    press("ArrowUp");
    expect(activeRow()).toBe(rows()[rows().length - 1]);
  });

  it("closes on Escape without clearing the draft", () => {
    render();
    type("/gr");
    press("Escape");
    expect(listbox()).toBeNull();
    expect(composerText()).toBe("/gr");
  });

  it("does not let that Escape reach the shell's cancel-turn handler", () => {
    // app-shell.tsx binds Esc on `window` to cancel the active turn; React
    // delegates below that, so dismissing the dropdown must stop propagation
    // or closing a suggestion list would kill an in-flight turn.
    const shellEsc = vi.fn();
    window.addEventListener("keydown", shellEsc);
    try {
      render();
      type("/gr");
      press("Escape");
      expect(shellEsc).not.toHaveBeenCalled();

      // …and with the list closed, Esc reaches the shell exactly as before.
      press("Escape");
      expect(shellEsc).toHaveBeenCalledTimes(1);
    } finally {
      window.removeEventListener("keydown", shellEsc);
    }
  });
});

describe("InputBar — Enter semantics (the regression guard)", () => {
  it("does NOT send while the dropdown is open", () => {
    const send = vi.fn();
    useChatStore.setState({ send });
    render();
    type("/gr");

    press("Enter");
    expect(send).not.toHaveBeenCalled();
  });

  it("sends on Enter once the dropdown is closed", () => {
    const send = vi.fn();
    useChatStore.setState({ send });
    render();
    type("just a message");

    press("Enter");
    expect(send).toHaveBeenCalledWith("just a message", SID, []);
  });

  it("still treats Shift+Enter as a newline, not a send", () => {
    const send = vi.fn();
    useChatStore.setState({ send });
    render();
    type("line one");

    const ev = press("Enter", { shiftKey: true });
    expect(send).not.toHaveBeenCalled();
    expect(ev.defaultPrevented).toBe(false);
  });

  it("honors the ctrlEnter send preference when the dropdown is closed", () => {
    const send = vi.fn();
    useChatStore.setState({ send });
    usePrefsStore.setState({ sendMessageKey: "ctrlEnter" });
    render();
    type("ship it");

    press("Enter");
    expect(send).not.toHaveBeenCalled();

    press("Enter", { metaKey: true });
    expect(send).toHaveBeenCalledWith("ship it", SID, []);
  });
});

describe("InputBar — accepting a slash command", () => {
  it("activates a skill and clears the token, without sending", () => {
    const spy = vi.spyOn(ipc, "activateSkill").mockResolvedValue();
    const send = vi.fn();
    useChatStore.setState({ send });
    render();
    type("/grill-me");

    press("Enter");
    expect(spy).toHaveBeenCalledWith("grill-me");
    expect(composerText()).toBe("");
    expect(send).not.toHaveBeenCalled();
  });

  it("skips the IPC for an already-active skill", () => {
    useSkillsStore.setState({ skills: [skill({ active: true })] });
    const spy = vi.spyOn(ipc, "activateSkill").mockResolvedValue();
    render();
    type("/grill-me");

    press("Enter");
    expect(spy).not.toHaveBeenCalled();
    expect(composerText()).toBe("");
  });

  it("expands a message shortcut into the composer instead of sending it", () => {
    const send = vi.fn();
    useChatStore.setState({ send });
    render();
    type("/ship");

    press("Tab");
    expect(composerText()).toBe("Open a PR and push.");
    expect(send).not.toHaveBeenCalled();
  });

  it("leaves `/goal` to the existing submit() grammar", () => {
    const startGoal = vi.fn();
    render();
    type("/goal");

    press("Enter");
    // Accepting only completes the token — the objective is still to be typed,
    // and parseGoalCommand handles it on submit as it did before.
    expect(composerText()).toBe("/goal ");
    expect(startGoal).not.toHaveBeenCalled();
    expect(listbox()).toBeNull();
  });
});

describe("InputBar — active-skill chips (#1036)", () => {
  it("shows a removable chip per active skill", () => {
    const spy = vi.spyOn(ipc, "deactivateSkill").mockResolvedValue();
    useSkillsStore.setState({ skills: [skill({ active: true })] });
    render();

    const remove = container.querySelector<HTMLButtonElement>(
      '[aria-label="Deactivate grill-me"]',
    );
    expect(remove).not.toBeNull();

    act(() => {
      remove?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(spy).toHaveBeenCalledWith("grill-me");
  });

  it("renders no chip row when nothing is active", () => {
    render();
    expect(container.querySelector('[aria-label^="Deactivate"]')).toBeNull();
  });

  it("explains instead of promising when the session is bound to a phenotype", () => {
    // `turn_active_skills` resolves a phenotype-bound session from its phenotype
    // and ignores the global active set, so chips would misrepresent reality.
    useChatStore.setState({
      sessions: [{ id: SID, title: "Test", phenotype: "reviewer" } as Session],
    });
    useSkillsStore.setState({ skills: [skill({ active: true })] });
    render();

    expect(container.querySelector('[aria-label^="Deactivate"]')).toBeNull();
    expect(container.textContent).toContain("reviewer");
    expect(container.textContent).toContain("phenotype");
  });

  it("marks skill rows as inapplicable on a phenotype-bound session", () => {
    useChatStore.setState({
      sessions: [{ id: SID, title: "Test", phenotype: "reviewer" } as Session],
    });
    render();
    type("/gr");
    expect(rows()[0].textContent).toContain("Won't apply");
  });
});
