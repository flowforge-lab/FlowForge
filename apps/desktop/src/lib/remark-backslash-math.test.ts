import { describe, expect, it } from "vitest";
import remarkParse from "remark-parse";
import { unified } from "unified";
import type { Nodes, Parents } from "mdast";

import { remarkBackslashMath } from "@/lib/remark-backslash-math";

const processor = unified().use(remarkParse).use(remarkBackslashMath);

function parse(source: string): Nodes {
  return processor.runSync(processor.parse(source), source) as Nodes;
}

/** Flatten the tree to `type:value` pairs — enough to assert node shape. */
function nodes(tree: Nodes): { type: string; value?: string }[] {
  const out: { type: string; value?: string }[] = [];
  const walk = (node: Nodes) => {
    out.push({
      type: node.type,
      value: (node as { value?: string }).value,
    });
    for (const child of (node as Parents).children ?? []) walk(child);
  };
  walk(tree);
  return out;
}

function typesOf(tree: Nodes): string[] {
  return nodes(tree).map((n) => n.type);
}

describe("remarkBackslashMath (#1102)", () => {
  it("converts \\(…\\) to inline math, keeping the surrounding text", () => {
    const tree = parse("Inline \\(A \\Rightarrow R\\) end.");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "paragraph", value: undefined },
      { type: "text", value: "Inline " },
      { type: "inlineMath", value: "A \\Rightarrow R" },
      { type: "text", value: " end." },
    ]);
  });

  it("lifts a standalone \\[…\\] into a block math node", () => {
    const tree = parse("\\[ A \\implies R \\]");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "math", value: "A \\implies R" },
    ]);
  });

  it("demotes a mid-sentence \\[…\\] to inline math", () => {
    const tree = parse("Given \\[ x^2 \\] we conclude.");

    expect(typesOf(tree)).toEqual([
      "root",
      "paragraph",
      "text",
      "inlineMath",
      "text",
    ]);
  });

  it("leaves inline code untouched", () => {
    const tree = parse("Use `\\(x\\)` and `\\[y\\]` literally.");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "paragraph", value: undefined },
      { type: "text", value: "Use " },
      { type: "inlineCode", value: "\\(x\\)" },
      { type: "text", value: " and " },
      { type: "inlineCode", value: "\\[y\\]" },
      { type: "text", value: " literally." },
    ]);
  });

  it("leaves fenced code untouched", () => {
    const tree = parse("```c\nint a[] = {1};\n// \\(not math\\)\n```");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "code", value: "int a[] = {1};\n// \\(not math\\)" },
    ]);
  });

  it("handles several formulas in one paragraph", () => {
    const tree = parse("\\(a\\) then \\(b\\)");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "paragraph", value: undefined },
      { type: "inlineMath", value: "a" },
      { type: "text", value: " then " },
      { type: "inlineMath", value: "b" },
    ]);
  });

  it("spans soft line breaks inside a paragraph", () => {
    const tree = parse("\\[ a +\nb \\]");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "math", value: "a +\nb" },
    ]);
  });

  it("keeps escapes in the surrounding text as remark would", () => {
    const tree = parse("A \\* star \\(x\\) here");

    expect(nodes(tree)).toEqual([
      { type: "root", value: undefined },
      { type: "paragraph", value: undefined },
      { type: "text", value: "A * star " },
      { type: "inlineMath", value: "x" },
      { type: "text", value: " here" },
    ]);
  });

  it("is a no-op on text with no backslash delimiters", () => {
    const source = "Plain prose with (parens) and [brackets].";
    expect(typesOf(parse(source))).toEqual(["root", "paragraph", "text"]);
  });

  it("is a no-op when nodes carry no position info", () => {
    const tree = {
      type: "root",
      children: [
        {
          type: "paragraph",
          children: [{ type: "text", value: "\\(x\\)" }],
        },
      ],
    };
    remarkBackslashMath()(
      tree as never,
      { toString: () => "\\(x\\)" } as never,
    );

    expect(typesOf(tree as Nodes)).toEqual(["root", "paragraph", "text"]);
  });
});
