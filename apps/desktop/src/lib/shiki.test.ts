// @vitest-environment jsdom
//
// #1169: FlowForge renders LLM output, so a fence's language is arbitrary —
// a model can write ```notareallanguage, ```JS, or nothing at all. The
// user-visible contract is that every one of those still renders the code, with
// the text intact, and that `highlight` never rejects (its callers `void` the
// promise, so a rejection would be an unhandled one rather than something the
// UI could act on).

import { describe, expect, it } from "vitest";

import { highlight } from "@/lib/shiki";

/** Flatten a hast tree back to its text, the way the copy button's
 *  `childrenToText` walk does over the rendered React tree. */
function textOf(node: unknown): string {
  const n = node as {
    type?: string;
    value?: string;
    children?: unknown[];
  };
  if (n.type === "text") return n.value ?? "";
  return (n.children ?? []).map(textOf).join("");
}

const tokenCount = (root: unknown): number => {
  let n = 0;
  const walk = (node: unknown) => {
    const el = node as {
      type?: string;
      properties?: { style?: string };
      children?: unknown[];
    };
    if (el.properties?.style?.includes("--shiki-token-")) n += 1;
    (el.children ?? []).forEach(walk);
  };
  walk(root);
  return n;
};

describe("highlight (#1169)", () => {
  it("highlights a known language", async () => {
    const root = await highlight("const x = 1;", "ts");
    expect(tokenCount(root)).toBeGreaterThan(0);
    expect(textOf(root)).toBe("const x = 1;");
  });

  it("accepts an alias and is case-insensitive", async () => {
    const root = await highlight("const y = 2;", "JS");
    expect(tokenCount(root)).toBeGreaterThan(0);
    expect(textOf(root)).toBe("const y = 2;");
  });

  it.each([
    ["a language that does not exist", "notareallanguage"],
    ["an empty language", ""],
    ["an explicit plain language", "text"],
  ])("renders %s without throwing, text intact", async (_label, lang) => {
    const code = "just some ¯\\_(ツ)_/¯ text";
    const root = await highlight(code, lang);
    expect(textOf(root)).toBe(code);
  });

  it("returns the identical cached tree for a repeated call", async () => {
    // Identity, not equality: the cache is what stops the virtualized
    // transcript re-highlighting (and re-flashing) on every remount.
    const a = await highlight("const z = 3;", "ts");
    const b = await highlight("const z = 3;", "ts");
    expect(b).toBe(a);
  });
});
