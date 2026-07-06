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
