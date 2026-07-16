// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { MarkdownEditor } from "@/components/ui/markdown-editor";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

function render(ui: React.ReactNode) {
  act(() => {
    root.render(ui);
  });
}

// Milkdown creates its editor asynchronously (`editor.create().then(...)`), so
// the ProseMirror DOM appears a few microtasks after render. Flush repeatedly
// until the editable surface mounts (or give up after a bounded number of ticks).
async function waitForEditor(): Promise<HTMLElement> {
  for (let i = 0; i < 50; i++) {
    const el = container.querySelector<HTMLElement>(".ProseMirror");
    if (el) return el;
    await act(async () => {
      await Promise.resolve();
    });
  }
  throw new Error("ProseMirror editor did not mount");
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("MarkdownEditor (#952)", () => {
  it("mounts an editable ProseMirror surface", async () => {
    render(<MarkdownEditor value="" onChange={() => {}} />);
    const editor = await waitForEditor();
    expect(editor.getAttribute("contenteditable")).toBe("true");
  });

  it("renders the initial value as content", async () => {
    render(<MarkdownEditor value="# Hello world" onChange={() => {}} />);
    const editor = await waitForEditor();
    expect(editor.querySelector("h1")?.textContent).toBe("Hello world");
  });

  it("is non-editable in read-only mode", async () => {
    render(
      <MarkdownEditor value="read only text" onChange={() => {}} readOnly />,
    );
    const editor = await waitForEditor();
    expect(editor.getAttribute("contenteditable")).toBe("false");
    expect(editor.textContent).toContain("read only text");
  });

  it("passes the placeholder text through as a CSS custom property", async () => {
    render(
      <MarkdownEditor value="" onChange={() => {}} placeholder="Write here…" />,
    );
    await waitForEditor();
    const wrapper = container.querySelector<HTMLElement>(
      '[data-slot="markdown-editor"]',
    );
    expect(wrapper?.style.getPropertyValue("--ff-md-placeholder")).toBe(
      '"Write here…"',
    );
  });
});
