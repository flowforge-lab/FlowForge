import { describe, expect, it } from "vitest";
import {
  CONTROL_DEFAULTS,
  MODE_COLUMNS,
  PERMISSION_ROWS,
  ROW_SAFETY,
  cellLabel,
  cellToMark,
  cycleCell,
  OVERRIDE_BUCKETS,
  groupOverridesByCell,
  bucketRowsByCell,
} from "./control";
import type {
  PermissionCell,
  PermissionOverrideEntry,
  Safety,
} from "@/bindings";

describe("control matrix metadata", () => {
  it("lists modes plan → auto → act and the five permission rows", () => {
    expect(MODE_COLUMNS.map((m) => m.value)).toEqual(["plan", "auto", "act"]);
    expect(PERMISSION_ROWS.map((r) => r.key)).toEqual([
      "read",
      "localWrites",
      "externalReads",
      "publish",
      "dangerous",
    ]);
  });
});

describe("live matrix mapping (#702)", () => {
  it("maps each presentation row to its backend Safety tier", () => {
    expect(ROW_SAFETY).toEqual({
      read: "readonly",
      localWrites: "write",
      externalReads: "sensitive",
      publish: "publish",
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
  it("has memory on and empty lists (default mode now lives in prefs/backend, #798)", () => {
    expect(CONTROL_DEFAULTS.injectMemory).toBe(true);
    expect(CONTROL_DEFAULTS.userInstructions).toBe("");
    expect(CONTROL_DEFAULTS.promptFiles).toEqual([]);
    // Default mode is no longer a control-config field.
    expect("defaultMode" in CONTROL_DEFAULTS).toBe(false);
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

// The mode pill posture hint (#801): the matrix cell is the advertise switch since
// #793/#795, so allow → auto-runs, ask → needs approval, deny → hidden.
describe("bucketRowsByCell (#801)", () => {
  // Row label lookup for readable expectations.
  const labels = (rows: { label: string }[]) => rows.map((r) => r.label);

  it("buckets Plan (read allow, sensitive ask, write/publish/dangerous deny)", () => {
    const plan: Record<Safety, PermissionCell> = {
      readonly: "allow",
      write: "deny",
      sensitive: "ask",
      dangerous: "deny",
      publish: "deny",
    };
    const b = bucketRowsByCell(plan);
    expect(labels(b.allow)).toEqual(["Read & browse"]);
    expect(labels(b.ask)).toEqual(["External reads"]);
    expect(labels(b.deny)).toEqual([
      "Local writes",
      "Publish / remote writes",
      "Dangerous commands",
    ]);
  });

  it("buckets Act (all allow except dangerous ask) — nothing hidden", () => {
    const act: Record<Safety, PermissionCell> = {
      readonly: "allow",
      write: "allow",
      sensitive: "allow",
      dangerous: "ask",
      publish: "allow",
    };
    const b = bucketRowsByCell(act);
    expect(labels(b.allow)).toEqual([
      "Read & browse",
      "Local writes",
      "External reads",
      "Publish / remote writes",
    ]);
    expect(labels(b.ask)).toEqual(["Dangerous commands"]);
    expect(b.deny).toEqual([]);
  });

  it("buckets Auto (sensitive/publish ask, dangerous deny)", () => {
    const auto: Record<Safety, PermissionCell> = {
      readonly: "allow",
      write: "allow",
      sensitive: "ask",
      dangerous: "deny",
      publish: "ask",
    };
    const b = bucketRowsByCell(auto);
    expect(labels(b.allow)).toEqual(["Read & browse", "Local writes"]);
    expect(labels(b.ask)).toEqual([
      "External reads",
      "Publish / remote writes",
    ]);
    expect(labels(b.deny)).toEqual(["Dangerous commands"]);
  });

  it("returns empty buckets when the matrix row is undefined", () => {
    expect(bucketRowsByCell(undefined)).toEqual({
      allow: [],
      ask: [],
      deny: [],
    });
  });
});
