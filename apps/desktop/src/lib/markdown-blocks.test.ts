import { describe, expect, it } from "vitest";

import { splitBlocks } from "@/lib/markdown-blocks";

describe("splitBlocks", () => {
  it("keeps a single paragraph as the open block when nothing follows it", () => {
    const { closed, open } = splitBlocks("hello world");
    expect(closed).toEqual([]);
    expect(open).toBe("hello world");
  });

  it("closes a paragraph once a blank line and more content follow", () => {
    const { closed, open } = splitBlocks("first paragraph\n\nsecond paragraph");
    expect(closed).toEqual(["first paragraph"]);
    expect(open).toBe("second paragraph");
  });

  it("closes multiple paragraphs in order", () => {
    const { closed, open } = splitBlocks("one\n\ntwo\n\nthree");
    expect(closed).toEqual(["one", "two"]);
    expect(open).toBe("three");
  });

  it("closes a paragraph as soon as its terminating blank line arrives", () => {
    // "two\n\n" contains a real blank line right after "two" (the trailing ""
    // is the in-progress cursor position for whatever comes next), so "two"
    // is a completed paragraph and the open block is empty.
    const { closed, open } = splitBlocks("one\n\ntwo\n\n");
    expect(closed).toEqual(["one", "two"]);
    expect(open).toBe("");
  });

  it("does not close a paragraph on a single trailing newline (no blank line yet)", () => {
    // Only one "\n" so far — the next token could still continue "two" on
    // the same line, so it must stay in the open block.
    const { closed, open } = splitBlocks("one\n\ntwo\n");
    expect(closed).toEqual(["one"]);
    expect(open).toBe("two\n");
  });

  it("never splits inside an open fenced code block", () => {
    const content = "```ts\nconst x = 1;\n\nconst y = 2;\n```\n\nafter";
    const { closed, open } = splitBlocks(content);
    expect(closed).toEqual(["```ts\nconst x = 1;\n\nconst y = 2;\n```"]);
    expect(open).toBe("after");
  });

  it("treats an unterminated fence as still open (never closed)", () => {
    const content = "before\n\n```ts\nconst x = 1;\n\nstill inside fence";
    const { closed, open } = splitBlocks(content);
    expect(closed).toEqual(["before"]);
    expect(open).toBe("```ts\nconst x = 1;\n\nstill inside fence");
  });

  it("handles longer backtick fences and tildes", () => {
    const content = "````md\n```\ncode\n```\n````\n\nafter";
    const { closed, open } = splitBlocks(content);
    expect(closed).toEqual(["````md\n```\ncode\n```\n````"]);
    expect(open).toBe("after");

    const tilde = splitBlocks("~~~\nfoo\n~~~\n\nbar");
    expect(tilde.closed).toEqual(["~~~\nfoo\n~~~"]);
    expect(tilde.open).toBe("bar");
  });

  it("keeps a GFM table as a single block (no internal blank lines)", () => {
    const table =
      "| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n\nafter table";
    const { closed, open } = splitBlocks(table);
    expect(closed).toEqual(["| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |"]);
    expect(open).toBe("after table");
  });

  it("keeps closed blocks stable across incremental appends", () => {
    const chunks = ["one\n\ntw", "o\n\nthre", "e"];
    let acc = "";
    let lastClosed: string[] = [];
    for (const chunk of chunks) {
      acc += chunk;
      const { closed } = splitBlocks(acc);
      // Every previously-closed block must still appear, unchanged, as a
      // prefix of the newly-computed closed list.
      for (let i = 0; i < lastClosed.length; i++) {
        expect(closed[i]).toBe(lastClosed[i]);
      }
      lastClosed = closed;
    }
    expect(lastClosed).toEqual(["one", "two"]);
  });
});
