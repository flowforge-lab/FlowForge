import { describe, expect, it } from "vitest";
import {
  CONTROL_DEFAULTS,
  MODE_CELLS,
  MODE_COLUMNS,
  PERMISSION_ROWS,
  cellToDecision,
  policyForMode,
  type DefaultMode,
} from "./control";

describe("control matrix metadata", () => {
  it("lists modes plan → auto → act and the four permission rows", () => {
    expect(MODE_COLUMNS.map((m) => m.value)).toEqual(["plan", "auto", "act"]);
    expect(PERMISSION_ROWS.map((r) => r.key)).toEqual([
      "read",
      "localWrites",
      "externalChanges",
      "dangerous",
    ]);
  });

  it("has a cell mark for every mode × row", () => {
    for (const mode of MODE_COLUMNS) {
      for (const row of PERMISSION_ROWS) {
        expect(MODE_CELLS[mode.value][row.key]).toBeDefined();
      }
    }
  });

  it("never silently allows dangerous commands", () => {
    const modes: DefaultMode[] = ["plan", "auto", "act"];
    for (const mode of modes) {
      expect(MODE_CELLS[mode].dangerous).not.toBe("check");
    }
  });
});

describe("cellToDecision / policyForMode", () => {
  it("maps marks to decisions", () => {
    expect(cellToDecision("check")).toBe("allow");
    expect(cellToDecision("cross")).toBe("deny");
    expect(cellToDecision("ask")).toBe("ask");
  });

  it("derives a per-row policy from a mode's cells", () => {
    expect(policyForMode("plan")).toEqual({
      read: "allow",
      localWrites: "deny",
      externalChanges: "deny",
      dangerous: "deny",
    });
    expect(policyForMode("act").externalChanges).toBe("allow");
    expect(policyForMode("act").dangerous).toBe("ask");
  });
});

describe("CONTROL_DEFAULTS", () => {
  it("defaults to auto with a matching policy, memory on, empty lists", () => {
    expect(CONTROL_DEFAULTS.defaultMode).toBe("auto");
    expect(CONTROL_DEFAULTS.permissionPolicy).toEqual(policyForMode("auto"));
    expect(CONTROL_DEFAULTS.injectMemory).toBe(true);
    expect(CONTROL_DEFAULTS.userInstructions).toBe("");
    expect(CONTROL_DEFAULTS.promptFiles).toEqual([]);
    expect(CONTROL_DEFAULTS.overrides).toEqual({
      denied: [],
      requireApproval: [],
      allowed: [],
    });
  });
});
