// @vitest-environment jsdom
//
// The second of the two gates on a stale persisted payload (#944 fallout).
//
// `parseContent` in `store/split.ts` is the first: it drops a content kind this
// build can't render on the way in, and `store/split.test.ts` pins it. This
// file pins the other one — the `switch` fallback in `SplitBody` — because the
// two are a pair and a mutant that reverts only the fallback survived the whole
// suite *and* `tsc`:
//
//     default: { const unreachable: never = content; return unreachable; }
//
// That is the line that actually crashed the app. React throws when handed an
// object as a child, and `SplitPanel` renders above the pane tree, so the
// throw took the entire window to the error boundary rather than blanking one
// panel. The compiler cannot catch it: `never` is a compile-time claim about
// what this build writes, and the object comes from a months-old profile.
//
// So the test drives the component with a payload the type system says is
// impossible — that's the point, and the cast is deliberate.

import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SplitPanel } from "@/components/split-panel";
import { useSplitStore, type SplitContent } from "@/store/split";

vi.mock("@/lib/ipc", () => ({ ipc: new Proxy({}, { get: () => vi.fn() }) }));

/** A shape this build has no case for — `{ kind: "files" }` is the real one,
 *  carrying `text` so it clears any parser check and reaches the renderer. */
const UNKNOWN_KIND = {
  kind: "files",
  text: "src/lib.rs",
} as unknown as SplitContent;

function openWith(content: SplitContent | null) {
  useSplitStore.setState({ open: true, width: 480, wrap: true, content });
}

afterEach(() => {
  useSplitStore.setState({ open: false, content: null });
});

describe("SplitPanel body fallback (#944 fallout)", () => {
  it("renders nothing for a content kind it has no case for, instead of throwing", () => {
    openWith(UNKNOWN_KIND);

    // The assertion is that this call returns at all. With the fallback
    // returning the object, React throws here and the app's error boundary
    // catches it one level up in production.
    expect(() => render(<SplitPanel />)).not.toThrow();
  });

  it("still renders the panel around the empty body", () => {
    openWith(UNKNOWN_KIND);

    const { container } = render(<SplitPanel />);

    // The panel chrome survives — a stale payload costs the body, not the
    // window. `getComputedStyle` is not consulted; presence is the claim.
    expect(container.querySelector("aside")).not.toBeNull();
    expect(container.querySelector('[title="Close (Esc)"]')).not.toBeNull();
    // And nothing stringified the object into the DOM.
    expect(container.textContent).not.toContain("files");
    expect(container.textContent).not.toContain("[object Object]");
  });

  it("renders the known kinds it does have cases for", () => {
    // The control: if this suite passed with a body that renders nothing at
    // all, the test above would prove nothing.
    openWith({ kind: "text", text: "tool output here" });
    const text = render(<SplitPanel />);
    expect(text.container.textContent).toContain("tool output here");
    text.unmount();

    openWith({ kind: "code", lang: "rust", text: "fn main() {}" });
    const code = render(<SplitPanel />);
    expect(code.container.querySelector("pre")).not.toBeNull();
  });

  it("shows the empty state when there is no content", () => {
    openWith(null);

    const { container } = render(<SplitPanel />);

    expect(container.textContent).toContain("Nothing open");
  });
});
