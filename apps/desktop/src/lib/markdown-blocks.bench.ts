// Lightweight before/after benchmark for #844: run with
//   pnpm vitest bench src/lib/markdown-blocks.bench.ts
//
// Mirrors the maintainer's issue-comment probe methodology (renderToStaticMarkup
// of the real react-markdown + remark-gfm pipeline, no rehype-highlight — the
// streaming config) at growing content sizes, comparing:
//   - old: render the whole accumulated message every frame
//   - new: splitBlocks + render only the small open tail every frame
// Numbers are machine-dependent — this is a dev-time sanity check backing the
// "measurement" acceptance criterion, not a CI gate. Split-boundary
// correctness is covered by markdown-blocks.test.ts.

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { bench, describe } from "vitest";

import { splitBlocks } from "@/lib/markdown-blocks";

function renderStreamingPipeline(text: string): string {
  return renderToStaticMarkup(
    createElement(ReactMarkdown, { remarkPlugins: [remarkGfm] }, text),
  );
}

// Synthetic reply: repeating prose + a fenced code block + a GFM table,
// grown to the target size. Mirrors the mixed content used in the issue's
// own probe.
function syntheticContent(targetChars: number): string {
  const unit =
    "This is a streamed sentence of prose that keeps growing as the model " +
    "replies with more detail and nuance.\n\n" +
    "```ts\nfunction add(a: number, b: number) {\n  return a + b;\n}\n```\n\n" +
    "| col a | col b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n\n";
  let out = "";
  while (out.length < targetChars) out += unit;
  return out.slice(0, targetChars);
}

const SIZES = [1000, 4000, 8000, 16000];

describe.each(SIZES)("frame cost at %i chars", (size) => {
  const content = syntheticContent(size);
  const { open } = splitBlocks(content);

  bench("old: re-render whole message every frame", () => {
    renderStreamingPipeline(content);
  });

  bench("new: splitBlocks + render only the open tail", () => {
    renderStreamingPipeline(open);
  });
});
