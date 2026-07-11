import { beforeEach, describe, expect, it } from "vitest";

import { useFindExpansion } from "@/store/find-expansion";

beforeEach(() => {
  useFindExpansion.setState({ forced: new Set<string>() });
});

describe("useFindExpansion (#875)", () => {
  it("starts empty", () => {
    expect(useFindExpansion.getState().forced.size).toBe(0);
  });

  it("forceOpenMany adds ids without dropping existing ones", () => {
    useFindExpansion.getState().forceOpenMany(["tool-step:c1"]);
    useFindExpansion.getState().forceOpenMany(["output:m7"]);
    expect([...useFindExpansion.getState().forced]).toEqual([
      "tool-step:c1",
      "output:m7",
    ]);
  });

  it("forceOpenMany is idempotent — re-adding does not change the set", () => {
    useFindExpansion.getState().forceOpenMany(["tool-step:c1"]);
    const before = useFindExpansion.getState().forced;
    useFindExpansion.getState().forceOpenMany(["tool-step:c1", "tool-step:c1"]);
    expect(useFindExpansion.getState().forced).toBe(before);
  });

  it("setForced replaces the entire set", () => {
    useFindExpansion.getState().forceOpenMany(["tool-step:c1", "output:m7"]);
    useFindExpansion.getState().setForced(["step-group:m1:s2"]);
    expect([...useFindExpansion.getState().forced]).toEqual([
      "step-group:m1:s2",
    ]);
  });

  it("setForced([]) clears without invoking per-id clearing", () => {
    useFindExpansion.getState().forceOpenMany(["tool-step:c1"]);
    useFindExpansion.getState().setForced([]);
    expect(useFindExpansion.getState().forced.size).toBe(0);
  });

  it("clear empties the set", () => {
    useFindExpansion.getState().forceOpenMany(["tool-step:c1"]);
    useFindExpansion.getState().clear();
    expect(useFindExpansion.getState().forced.size).toBe(0);
  });
});
