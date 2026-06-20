import { describe, expect, it } from "vitest";

import { modeForHotkey, MODE_META } from "@/lib/mode";

describe("modeForHotkey (#267)", () => {
  it("maps P/T/O to Plan/Act/Auto, case-insensitively", () => {
    expect(modeForHotkey("p")).toBe("plan");
    expect(modeForHotkey("P")).toBe("plan");
    expect(modeForHotkey("t")).toBe("act");
    expect(modeForHotkey("o")).toBe("auto");
  });

  it("returns undefined for unmapped keys", () => {
    expect(modeForHotkey("k")).toBeUndefined();
    expect(modeForHotkey("1")).toBeUndefined();
  });
});

describe("MODE_META", () => {
  it("has colour-coded metadata for every mode", () => {
    expect(MODE_META.plan.pillClass).toMatch(/blue/);
    expect(MODE_META.act.pillClass).toMatch(/emerald/);
    expect(MODE_META.auto.pillClass).toMatch(/amber/);
  });
});
