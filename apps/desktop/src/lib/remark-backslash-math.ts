// Backslash math delimiters for the markdown pipeline (#1102). `remark-math`
// only understands `$…$` / `$$…$$`, but FlowForge is a multi-provider harness
// and OpenAI-family models emit `\(…\)` / `\[…\]` by default — so dollar-only
// support would leave math broken on a large share of the main path.
//
// This must NOT be done by regex-replacing the raw source: `\(` and `\[` also
// occur legitimately inside code blocks and inline code (regex snippets, C,
// LaTeX examples), and a string replace would corrupt them. Visiting `text`
// nodes structurally excludes `code`/`inlineCode`, so that is where we work.
//
// The catch — and the reason this reads the source rather than `node.value`:
// CommonMark consumes `\(`, `\)`, `\[`, `\]` as *character escapes*, so by the
// time an mdast `text` node exists the backslashes are already gone:
//
//   source:    Inline \(A \Rightarrow R\) end.
//   text node: "Inline (A \Rightarrow R) end."
//
// Matching the node value would therefore mean matching bare `(`…`)` and
// `[`…`]`, which is far too loose (`[…]` also collides with shortcut link
// references). Instead we slice the ORIGINAL source back out using the node's
// `position` offsets, which still has the delimiters intact, and scan that.
// A node without position info (e.g. synthesized by another plugin) is skipped
// rather than guessed at, so the worst case is today's behaviour.

import { visit } from "unist-util-visit";
import type { Root, RootContent, Text } from "mdast";

// `\(inline\)` or `\[display\]`, non-greedy, `s` so display math may span the
// soft line breaks inside a single paragraph.
const BACKSLASH_MATH = /\\\((.+?)\\\)|\\\[(.+?)\\\]/gs;

// Cheap pre-filter so the common (math-free) text node costs one short regex
// test instead of a full delimiter walk.
const HAS_DELIMITER = /\\[([]/;

// CommonMark character escape: a backslash before ASCII punctuation. Applied to
// the plain-text segments around the math so they match the value remark itself
// would have produced from the same source.
const ESCAPED_PUNCTUATION = /\\([!-/:-@[-`{-~])/g;

/** mdast nodes contributed by `mdast-util-math`; not part of the base types. */
interface MathNode {
  type: "math" | "inlineMath";
  value: string;
  data: unknown;
  position?: Text["position"];
}

function unescape(raw: string): string {
  return raw.replace(ESCAPED_PUNCTUATION, "$1");
}

// `mdast-util-math` attaches the hast shape at parse time rather than through a
// mdast-to-hast handler, so nodes we synthesize have to carry the same `data` or
// they fall through to the unknown-node handler and render as plain text (and
// `rehype-katex`, which keys off the `math-inline`/`math-display` classes, never
// sees them). Kept byte-identical to that package's `enter*`/`exit*` handlers.
function mathNode(value: string, display: boolean): MathNode {
  const code = {
    type: "element",
    tagName: "code",
    properties: {
      className: ["language-math", display ? "math-display" : "math-inline"],
    },
    children: [{ type: "text", value }],
  };
  return {
    type: display ? "math" : "inlineMath",
    value,
    data: display
      ? { hName: "pre", hChildren: [code] }
      : {
          hName: "code",
          hProperties: code.properties,
          hChildren: code.children,
        },
  };
}

/** Convert one text node's source slice into text + math nodes, or `[]`. */
function splitMath(raw: string): RootContent[] {
  const out: RootContent[] = [];
  let cursor = 0;

  BACKSLASH_MATH.lastIndex = 0;
  for (
    let match = BACKSLASH_MATH.exec(raw);
    match !== null;
    match = BACKSLASH_MATH.exec(raw)
  ) {
    const [whole, inline, display] = match;
    const before = raw.slice(cursor, match.index);
    if (before) out.push({ type: "text", value: unescape(before) });

    out.push(
      mathNode((inline ?? display).trim(), display !== undefined) as never,
    );
    cursor = match.index + whole.length;
  }

  if (out.length === 0) return out;
  const after = raw.slice(cursor);
  if (after) out.push({ type: "text", value: unescape(after) });
  return out;
}

/** True when this node came from a `$$…$$` span in the source. */
function isDoubleDollar(node: MathNode, source: string): boolean {
  const start = node.position?.start.offset;
  return start !== undefined && source.startsWith("$$", start);
}

function asMath(node: RootContent | undefined): MathNode | undefined {
  const type = node?.type as string | undefined;
  return type === "math" || type === "inlineMath"
    ? (node as unknown as MathNode)
    : undefined;
}

/**
 * Remark plugin converting `\(…\)` to inline math and `\[…\]` to display math,
 * emitting the same `inlineMath`/`math` nodes `remark-math` produces so
 * `rehype-katex` typesets them identically.
 *
 * A formula that is the entire content of its paragraph is rendered as display
 * math and lifted out of the paragraph — display math lowers to `<pre>`, which
 * may not sit inside a `<p>`. That lift also covers a single-line `$$…$$`,
 * which `remark-math` classifies as *inline* math because its flow (display)
 * form requires the fences on their own lines; a formula alone on a line is
 * meant to be centred. A `\[…\]` appearing mid-sentence degrades to inline.
 */
export function remarkBackslashMath() {
  // `file` is the unified VFile; only its source text is needed, so it is typed
  // structurally rather than pulling `vfile` in as a direct dependency.
  return function transformer(tree: Root, file: { toString(): string }): void {
    const source = String(file);
    if (!source) return;

    visit(tree, "text", (node: Text, index, parent) => {
      if (!parent || index === undefined) return;

      const start = node.position?.start.offset;
      const end = node.position?.end.offset;
      if (start === undefined || end === undefined) return;

      const raw = source.slice(start, end);
      if (!HAS_DELIMITER.test(raw)) return;

      const replacement = splitMath(raw);
      if (replacement.length === 0) return;

      parent.children.splice(
        index,
        1,
        ...(replacement as (typeof parent.children)[number][]),
      );
      // Skip the nodes we just inserted.
      return index + replacement.length;
    });

    // Currency guard. `remark-math` accepts any `$…$` span, so ordinary prose
    // like "costs $5 to $10 today" is swallowed as a formula. Reject a single-
    // dollar span whose content is padded with whitespace — the same rule
    // markdown-it's math plugins use, and one real formulas never trip.
    visit(tree, "inlineMath", (node, index, parent) => {
      if (!parent || index === undefined) return;
      const start = node.position?.start.offset;
      const end = node.position?.end.offset;
      if (start === undefined || end === undefined) return;

      const raw = source.slice(start, end);
      if (raw.startsWith("$$") || !/^\$\s|\s\$$/.test(raw)) return;

      parent.children.splice(index, 1, {
        type: "text",
        value: raw,
      } as (typeof parent.children)[0]);
      return index + 1;
    });

    visit(tree, "paragraph", (paragraph, index, parent) => {
      if (!parent || index === undefined) return;

      const sole =
        paragraph.children.length === 1
          ? asMath(paragraph.children[0])
          : undefined;
      // Centre a formula that occupies its whole paragraph: our own `\[…\]`
      // nodes, and a single-line `$$…$$`, which remark-math classifies as
      // inline because its display form needs the fences on their own lines.
      const only =
        sole?.type === "math" ||
        (sole !== undefined && isDoubleDollar(sole, source))
          ? sole
          : undefined;

      if (only) {
        // Standalone: render as display math in place of the paragraph. A
        // `$$…$$` written inline mid-sentence keeps remark-math's inline form.
        parent.children.splice(
          index,
          1,
          mathNode(only.value, true) as (typeof parent.children)[0],
        );
        return index + 1;
      }

      // Not standalone — demote any display math so no `<pre>` nests in a `<p>`.
      paragraph.children = paragraph.children.map((child) => {
        const math = asMath(child);
        return math?.type === "math"
          ? (mathNode(math.value, false) as (typeof paragraph.children)[0])
          : child;
      });
    });
  };
}
