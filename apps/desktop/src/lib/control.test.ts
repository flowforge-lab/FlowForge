import { describe, expect, it } from "vitest";
import {
  CONTROL_DEFAULTS,
  MODE_CELLS,
  MODE_COLUMNS,
  PERMISSION_ROWS,
  ROW_SAFETY,
  cellLabel,
  cellToDecision,
  cellToMark,
  cycleCell,
  policyForMode,
  OVERRIDE_BUCKETS,
  groupOverridesByCell,
  type DefaultMode,
} from "./control";
import type { PermissionCell, PermissionOverrideEntry } from "@/bindings";

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

describe("live matrix mapping (#702)", () => {
  it("maps each presentation row to its backend Safety tier", () => {
    expect(ROW_SAFETY).toEqual({
      read: "readonly",
      localWrites: "write",
      externalChanges: "sensitive",
      dangerous: "dangerous",
    });
    // Every rendered row has a Safety mapping.
    for (const row of PERMISSION_ROWS) {
      expect(ROW_SAFETY[row.key]).toBeDefined();
    }
  });

  it("renders backend cells with the CellMarkIcon vocabulary", () => {
    expect(cellToMark("allow")).toBe("check");
    expect(cellToMark("ask")).toBe("ask");
    expect(cellToMark("deny")).toBe("cross");
  });

  it("cycles Allow → Ask → Deny → Allow", () => {
    expect(cycleCell("allow")).toBe("ask");
    expect(cycleCell("ask")).toBe("deny");
    expect(cycleCell("deny")).toBe("allow");
    // Three clicks return to the start.
    const start: PermissionCell = "allow";
    expect(cycleCell(cycleCell(cycleCell(start)))).toBe(start);
  });

  it("labels every cell state", () => {
    expect(cellLabel("allow")).toBe("Allowed");
    expect(cellLabel("ask")).toBe("Ask first");
    expect(cellLabel("deny")).toBe("Denied");
  });
});

describe("CONTROL_DEFAULTS", () => {
  it("defaults to auto with a matching policy, memory on, empty lists", () => {
    expect(CONTROL_DEFAULTS.defaultMode).toBe("auto");
    expect(CONTROL_DEFAULTS.permissionPolicy).toEqual(policyForMode("auto"));
    expect(CONTROL_DEFAULTS.injectMemory).toBe(true);
    expect(CONTROL_DEFAULTS.userInstructions).toBe("");
    expect(CONTROL_DEFAULTS.promptFiles).toEqual([]);
  });
});

describe("override buckets", () => {
  it("orders buckets Deny → Ask → Allow", () => {
    expect(OVERRIDE_BUCKETS.map((b) => b.cell)).toEqual([
      "deny",
      "ask",
      "allow",
    ]);
  });

  it("groups the flat override list by cell", () => {
    const overrides: PermissionOverrideEntry[] = [
      { tool: "web_fetch", cell: "deny" },
      { tool: "git_push", cell: "ask" },
      { tool: "ls", cell: "allow" },
      { tool: "rm", cell: "deny" },
    ];
    expect(groupOverridesByCell(overrides)).toEqual({
      deny: ["web_fetch", "rm"],
      ask: ["git_push"],
      allow: ["ls"],
    });
  });

  it("returns empty buckets for no overrides", () => {
    expect(groupOverridesByCell([])).toEqual({
      allow: [],
      ask: [],
      deny: [],
    });
  });
});
