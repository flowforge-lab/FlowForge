// @vitest-environment jsdom
//
// Streaming-vs-final equivalence (#844). The streaming path splits content
// into memoized closed blocks + an open tail (see markdown-blocks.ts) so
// only the tail re-parses each frame; the final (non-streaming) path still
// runs one full parse over the whole message with highlighting. These two
// paths must produce structurally equivalent markdown once a message is
// fully delivered — streaming must never leave the user with a different
// final rendering than before this change.

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { Markdown } from "@/components/markdown";

afterEach(() => cleanup());

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

  it("only applies syntax-highlight spans once settled, never while streaming", () => {
    const content = "```ts\nconst x = 1;\n```";

    // rehype-highlight injects nested `hljs-*` spans for syntax tokens; the
    // outer `<code class="hljs">` wrapper is applied unconditionally by
    // COMPONENTS, so the token spans are the actual highlighting signal.
    const { container: streaming } = render(
      <Markdown content={content} streaming />,
    );
    expect(streaming.querySelector('span[class*="hljs-"]')).toBeNull();

    const { container: final } = render(
      <Markdown content={content} streaming={false} />,
    );
    expect(final.querySelector('span[class*="hljs-"]')).not.toBeNull();
  });
});
