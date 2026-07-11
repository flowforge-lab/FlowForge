// @vitest-environment jsdom

import { afterEach, describe, expect, it } from "vitest";
import { collectOccurrences, indexOfMessage } from "@/lib/find-highlight";

let root: HTMLElement;

function mount(html: string): HTMLElement {
  root = document.createElement("div");
  root.innerHTML = html;
  document.body.appendChild(root);
  return root;
}

afterEach(() => {
  root?.remove();
});

describe("collectOccurrences + indexOfMessage (#679/#710)", () => {
  const html = `
    <div data-message-id="m1">alpha beta alpha</div>
    <div data-message-id="m2">gamma alpha</div>
    <div data-message-id="m3">delta</div>
  `;

  it("collects one range per case-insensitive occurrence in document order", () => {
    const el = mount(html);
    const ranges = collectOccurrences(el, new Set(["m1", "m2", "m3"]), "alpha");
    // Two in m1, one in m2, none in m3.
    expect(ranges).toHaveLength(3);
  });

  it("indexOfMessage points at the first range inside the given message", () => {
    const el = mount(html);
    const ranges = collectOccurrences(el, new Set(["m1", "m2", "m3"]), "alpha");
    // m1 owns ranges 0 and 1; m2 owns range 2.
    expect(indexOfMessage(ranges, "m1")).toBe(0);
    expect(indexOfMessage(ranges, "m2")).toBe(2);
    expect(indexOfMessage(ranges, "m3")).toBe(-1);
    expect(indexOfMessage(ranges, "nope")).toBe(-1);
  });

  it("only scans the messages it is given", () => {
    const el = mount(html);
    const ranges = collectOccurrences(el, new Set(["m2"]), "alpha");
    expect(ranges).toHaveLength(1);
    expect(indexOfMessage(ranges, "m2")).toBe(0);
    expect(indexOfMessage(ranges, "m1")).toBe(-1);
  });
});

describe("collectOccurrences token-awareness (#748)", () => {
  it("highlights every token of a multi-word query in document order", () => {
    const el = mount(`<div data-message-id="m1">run the turn then run</div>`);
    // Two `run` + one `turn` = 3 whole-token hits, ordered by offset.
    const ranges = collectOccurrences(el, new Set(["m1"]), "run turn");
    expect(ranges.map((r) => r.toString())).toEqual(["run", "turn", "run"]);
  });

  it("does not highlight a token inside a larger word", () => {
    const el = mount(`<div data-message-id="m1">overrun running run</div>`);
    // Only the standalone `run` matches — not `overrun`/`running`.
    const ranges = collectOccurrences(el, new Set(["m1"]), "run");
    expect(ranges).toHaveLength(1);
    expect(ranges[0].toString()).toBe("run");
  });

  it("matches tokens case-insensitively", () => {
    const el = mount(`<div data-message-id="m1">RUN Turn</div>`);
    const ranges = collectOccurrences(el, new Set(["m1"]), "run TURN");
    expect(ranges.map((r) => r.toString())).toEqual(["RUN", "Turn"]);
  });

  it("de-duplicates repeated query tokens", () => {
    const el = mount(`<div data-message-id="m1">run run</div>`);
    const ranges = collectOccurrences(el, new Set(["m1"]), "run run");
    expect(ranges).toHaveLength(2); // two occurrences, not four
  });

  it("returns [] for a blank or punctuation-only query", () => {
    const el = mount(`<div data-message-id="m1">run turn</div>`);
    expect(collectOccurrences(el, new Set(["m1"]), "   ")).toHaveLength(0);
    expect(collectOccurrences(el, new Set(["m1"]), "-.")).toHaveLength(0);
  });
});

describe("collectOccurrences data-skip-find (#875)", () => {
  it("excludes a match whose only occurrence is under a data-skip-find sibling", () => {
    // Mirrors ThinkingBlock's collapsed state (#901 review): the skip-marked
    // text is a *sibling* of other content in the row, not nested inside a
    // shared wrapper — the TreeWalker must still reject it on its own, not
    // rely on some ancestor-level opt-out.
    const el = mount(`
      <div data-message-id="m1">
        <span data-skip-find>alpha preview</span>
        <span>beta body</span>
      </div>
    `);
    const ranges = collectOccurrences(el, new Set(["m1"]), "alpha");
    expect(ranges).toHaveLength(0);
  });

  it("still finds a match outside the data-skip-find sibling in the same row", () => {
    const el = mount(`
      <div data-message-id="m1">
        <span data-skip-find>alpha preview</span>
        <span>alpha body</span>
      </div>
    `);
    const ranges = collectOccurrences(el, new Set(["m1"]), "alpha");
    expect(ranges).toHaveLength(1);
  });
});
