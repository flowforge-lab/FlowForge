// @vitest-environment jsdom
//
// Streaming-vs-final equivalence (#844). The streaming path splits content
// into memoized closed blocks + an open tail (see markdown-blocks.ts) so
// only the tail re-parses each frame; the final (non-streaming) path still
// runs one full parse over the whole message with highlighting. These two
// paths must produce structurally equivalent markdown once a message is
// fully delivered — streaming must never leave the user with a different
// final rendering than before this change.

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Markdown } from "@/components/markdown";

afterEach(() => cleanup());

// A token Shiki has coloured, i.e. proof that highlighting has actually run.
const TOKEN_SPAN = 'span[style*="--shiki-token-"]';

/** Let every pending microtask and macrotask drain, inside `act` so React has
 *  flushed any state update they produced. Unlike `waitFor`, this does not stop
 *  at the first success — which is what a "never happens" assertion needs. */
async function flushAsync(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

// Simulates feeding `content` to the streaming renderer token-by-token in
// `chunkSize`-character increments, mirroring how `applyToken` grows the
// message content one batched delta at a time.
function renderStreamedIncrementally(content: string, chunkSize: number) {
  let acc = "";
  let container: HTMLElement | null = null;
  for (let i = 0; i < content.length; i += chunkSize) {
    acc += content.slice(i, i + chunkSize);
    cleanup();
    ({ container } = render(<Markdown content={acc} streaming />));
  }
  return container!;
}

// Block-splitting renders closed blocks as separate ReactMarkdown instances,
// each producing its own top-level block element (<p>, <ul>, <table>, ...)
// directly under the ".ff-prose" wrapper — same as a single parse tree would,
// since react-markdown also emits one top-level element per block. Comparing
// per-block text (rather than the whole container's textContent) avoids
// false negatives from whitespace-only text nodes react-markdown inserts
// between siblings within one parse tree — invisible in rendering, since
// block elements lay out on their own line regardless of surrounding
// whitespace, but not present across separate instance boundaries.
function blockTexts(container: Element): string[] {
  const prose = container.querySelector(".ff-prose")!;
  return Array.from(prose.children).map((child) =>
    (child.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

describe("Markdown streaming vs final equivalence (#844)", () => {
  it("renders identical structure for prose + list once settled", () => {
    const content = "**bold** intro\n\nA list:\n\n- one\n- two\n- three";

    const streamed = renderStreamedIncrementally(content, 7);
    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );

    expect(streamed.querySelectorAll("strong").length).toBe(
      final.querySelectorAll("strong").length,
    );
    expect(streamed.querySelectorAll("li").length).toBe(
      final.querySelectorAll("li").length,
    );
    expect(blockTexts(streamed)).toEqual(blockTexts(final));
  });

  it("renders identical structure for a fenced code block + trailing paragraph", () => {
    const content =
      "Here is code:\n\n```ts\nfunction add(a: number, b: number) {\n  return a + b;\n}\n```\n\nDone.";

    const streamed = renderStreamedIncrementally(content, 11);
    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );

    expect(streamed.querySelectorAll("pre").length).toBe(
      final.querySelectorAll("pre").length,
    );
    expect(blockTexts(streamed)).toEqual(blockTexts(final));
  });

  it("renders identical structure for a GFM table", () => {
    const content =
      "| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n\nAfter the table.";

    const streamed = renderStreamedIncrementally(content, 9);
    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );

    expect(streamed.querySelectorAll("table").length).toBe(1);
    expect(final.querySelectorAll("table").length).toBe(1);
    expect(streamed.querySelectorAll("tr").length).toBe(
      final.querySelectorAll("tr").length,
    );
    expect(blockTexts(streamed)).toEqual(blockTexts(final));
  });

  it("only applies syntax-highlight spans once settled, never while streaming", async () => {
    const content = "```ts\nconst x = 1;\n```";

    // Shiki colours each token with an inline `var(--shiki-token-*)`; the outer
    // `<code class="shiki">` wrapper is emitted either way, so the coloured
    // token spans are the actual highlighting signal.
    //
    // Ordering here is load-bearing, and the reason is worth spelling out: an
    // assertion that the streaming render has no token spans is *vacuous* if it
    // runs before highlighting could have happened at all. Grammars arrive via
    // `await import("shiki")`, so the first paint is unhighlighted whether or
    // not `CodeBody`'s streaming guard exists — and `waitFor` cannot rescue it,
    // because `waitFor` returns the moment its callback first succeeds, which
    // for a negative is immediately. Both forms pass with the guard deleted.
    //
    // So: settle the final render FIRST. That is a positive condition `waitFor`
    // can legitimately synchronise on, and reaching it proves the grammar is
    // loaded and the result is in `lib/shiki.ts`'s cache under this exact
    // (lang, code) key.
    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );
    // Plain text first — the highlighted tree replaces it in place, so the
    // text must already be correct before the swap (no spinner, no reflow).
    expect(final.textContent).toContain("const x = 1;");
    await waitFor(() => {
      expect(final.querySelector(TOKEN_SPAN)).not.toBeNull();
    });
    expect(final.textContent).toContain("const x = 1;");

    // Now a streaming render of the same block, against that warm cache.
    // `ShikiCodeInner` seeds its state from `getCachedHighlight`, so without the
    // guard this would paint highlighted on its very first frame — no awaiting
    // required, and no timing to get wrong. The assertion is synchronous
    // precisely so it cannot pass by looking too early.
    const { container: streaming } = render(
      <Markdown content={content} streaming />,
    );
    expect(streaming.querySelector(TOKEN_SPAN)).toBeNull();
    expect(streaming.textContent).toContain("const x = 1;");

    // And it must still be unhighlighted after the render has had every chance
    // to settle — this catches a guard that merely defers rather than suppresses.
    await flushAsync();
    expect(streaming.querySelector(TOKEN_SPAN)).toBeNull();
  });

  // The transcript is virtualized (#1143), so a block unmounts and remounts as
  // the user scrolls past it. lib/shiki.ts caches the highlighted tree so the
  // remount paints highlighted on the first frame instead of flashing plain
  // text — assert that synchronously, with no await.
  it("re-renders an already-highlighted block without a plain-text flash", async () => {
    const content = "```ts\nconst cached = 42;\n```";

    const first = render(<Markdown content={content} />);
    await waitFor(() => {
      expect(first.container.querySelector(TOKEN_SPAN)).not.toBeNull();
    });
    cleanup();

    const { container } = render(<Markdown content={content} />);
    expect(container.querySelector(TOKEN_SPAN)).not.toBeNull();
  });
});

// KaTeX typesetting (#1102). Both delimiter families must work: models in the
// OpenAI family emit `\(…\)` / `\[…\]` rather than `$…$`, so dollar-only
// support would leave math broken across a large share of providers.
describe("Markdown math rendering (#1102)", () => {
  function render1(content: string, streaming = false) {
    return render(<Markdown content={content} streaming={streaming} />)
      .container;
  }

  it("typesets $$…$$ as display math", () => {
    const container = render1("$$\\frac{a}{b}$$");

    expect(container.querySelector(".katex-display")).not.toBeNull();
    expect(container.textContent).not.toContain("$$");
  });

  it("typesets $…$ as inline math", () => {
    const container = render1("Attraction $A$ holds.");

    expect(container.querySelector(".katex")).not.toBeNull();
    expect(container.querySelector(".katex-display")).toBeNull();
    expect(container.textContent).not.toContain("$");
  });

  it("typesets \\(…\\) as inline math", () => {
    const container = render1("Inline \\(A \\Rightarrow R\\) and done.");

    expect(container.querySelector(".katex")).not.toBeNull();
    expect(container.querySelector(".katex-display")).toBeNull();
    expect(container.textContent).toContain("done.");
  });

  it("typesets \\[…\\] as display math", () => {
    const container = render1("\\[ A \\implies R \\]");

    expect(container.querySelector(".katex-display")).not.toBeNull();
  });

  // Load-bearing: this is what the "text nodes only" rule in
  // remark-backslash-math.ts exists to protect. A naive string replace on the
  // raw markdown would corrupt every regex/C/LaTeX snippet a model writes.
  it("leaves backslash delimiters inside a fenced code block verbatim", () => {
    const container = render1(
      "```c\nint a[] = {1};\n// see \\(ref\\) and \\[eq\\]\n```",
    );

    const code = container.querySelector("pre code");
    expect(code).not.toBeNull();
    expect(code!.textContent).toContain("\\(ref\\)");
    expect(code!.textContent).toContain("\\[eq\\]");
    expect(container.querySelector(".katex")).toBeNull();
  });

  it("leaves backslash delimiters inside inline code verbatim", () => {
    const container = render1("Match `\\(x\\)` with `\\[y\\]`.");

    expect(container.querySelector(".katex")).toBeNull();
    expect(container.textContent).toContain("\\(x\\)");
    expect(container.textContent).toContain("\\[y\\]");
  });

  // remark-math rejects a `$…$` span whose content starts or ends with a space,
  // which is what keeps ordinary currency prose from being swallowed as math.
  it("does not treat currency amounts as math", () => {
    const container = render1("It costs $5 to $10 today.");

    expect(container.querySelector(".katex")).toBeNull();
    expect(container.textContent).toContain("$5 to $10");
  });

  it("renders math identically while streaming and once settled", () => {
    const content =
      "Given \\(A \\Rightarrow R\\), we get $x^2$.\n\n$$\\frac{a}{b}$$";

    const streamed = renderStreamedIncrementally(content, 6);
    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );

    expect(streamed.querySelectorAll(".katex").length).toBe(
      final.querySelectorAll(".katex").length,
    );
    expect(streamed.querySelectorAll(".katex-display").length).toBe(
      final.querySelectorAll(".katex-display").length,
    );
    expect(blockTexts(streamed)).toEqual(blockTexts(final));
  });
});

// Markdown link click handling (#1129). In packaged Tauri the bare
// `target="_blank"` anchor stays inside the webview; the renderer now routes
// http(s)/mailto clicks through `openExternalUrl` (mock path → `window.open`).
// Relative/anchor links must pass through untouched.
describe("Markdown anchor clicks (#1129)", () => {
  it("routes http(s)/mailto clicks through openExternalUrl", () => {
    const openSpy = vi.spyOn(globalThis, "open").mockImplementation(() => null);

    const { container } = render(
      <Markdown content="See [docs](https://example.com/docs) and [mail](mailto:hi@example.com)." />,
    );

    const links = Array.from(
      container.querySelectorAll(".ff-prose a"),
    ) as HTMLAnchorElement[];
    expect(links).toHaveLength(2);

    const [httpsLink, mailtoLink] = links;
    expect(httpsLink.getAttribute("href")).toBe("https://example.com/docs");
    expect(mailtoLink.getAttribute("href")).toBe("mailto:hi@example.com");

    fireEvent.click(httpsLink);
    fireEvent.click(mailtoLink);

    expect(openSpy).toHaveBeenCalledTimes(2);
    expect(openSpy).toHaveBeenNthCalledWith(
      1,
      "https://example.com/docs",
      "_blank",
      expect.stringContaining("noopener"),
    );
    expect(openSpy).toHaveBeenNthCalledWith(
      2,
      "mailto:hi@example.com",
      "_blank",
      expect.stringContaining("noopener"),
    );

    openSpy.mockRestore();
  });

  it("does not intercept relative or anchor links", () => {
    const openSpy = vi.spyOn(globalThis, "open").mockImplementation(() => null);

    const { container } = render(
      <Markdown content="See [/path](/path) and [section](#section)." />,
    );

    const links = Array.from(
      container.querySelectorAll(".ff-prose a"),
    ) as HTMLAnchorElement[];
    expect(links).toHaveLength(2);

    const click = (el: HTMLAnchorElement) => {
      const event = new MouseEvent("click", {
        bubbles: true,
        cancelable: true,
      });
      el.dispatchEvent(event);
      return event;
    };

    const relativeEvent = click(links[0]);
    const anchorEvent = click(links[1]);

    expect(openSpy).not.toHaveBeenCalled();
    expect(relativeEvent.defaultPrevented).toBe(false);
    expect(anchorEvent.defaultPrevented).toBe(false);

    openSpy.mockRestore();
  });
});
