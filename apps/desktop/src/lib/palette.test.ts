import { describe, it, expect } from "vitest";
import {
  buildCommands,
  buildMcpServerCommands,
  buildPhenotypeCommands,
  buildSkillCommands,
  fuzzyScore,
  filterCommands,
  mergePaletteResults,
} from "@/lib/palette";
import type { Phenotype, Session, SkillInfo } from "@/bindings";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

function session(partial: Partial<Session> & { id: string }): Session {
  return {
    goal: null,
    title: null,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

describe("fuzzyScore", () => {
  it("returns null when the query isn't a subsequence", () => {
    expect(fuzzyScore("xyz", "Toggle split panel")).toBeNull();
  });

  it("matches a scattered subsequence", () => {
    expect(fuzzyScore("tsp", "toggle split panel")).not.toBeNull();
  });

  it("is case-insensitive", () => {
    expect(fuzzyScore("SPLIT", "toggle split panel")).toBe(
      fuzzyScore("split", "toggle split panel"),
    );
  });

  it("scores a contiguous, word-boundary match higher than a scattered one", () => {
    const contiguous = fuzzyScore("split", "split")!;
    const scattered = fuzzyScore("split", "sxpxlxixt")!;
    expect(contiguous).toBeGreaterThan(scattered);
  });
});

describe("buildCommands", () => {
  const sessions = [
    session({ id: "a" }), // active
    session({ id: "b", goal: "Write docs" }),
    session({ id: "c", title: "Renamed C" }), // has a server-truth title
  ];
  const built = buildCommands({
    sessions,
    activeSessionId: "a",
  });
  const byId = new Map(built.map((c) => [c.id, c] as const));

  it("lists the quick actions first, in order", () => {
    expect(built.slice(0, 5).map((c) => c.id)).toEqual([
      "action:new-session",
      "action:toggle-split",
      "action:toggle-wrap",
      "action:open-files",
      "action:focus-composer",
    ]);
  });

  it("excludes the active session from the switch list", () => {
    const switchIds = built
      .filter((c) => c.kind === "switch-session")
      .map((c) => c.id);
    expect(switchIds).toEqual(["session:b", "session:c"]); // "a" excluded
  });

  it("labels switch commands via resolveLabel (title beats goal)", () => {
    expect(byId.get("session:b")?.title).toBe("Write docs"); // from goal
    expect(byId.get("session:c")?.title).toBe("Renamed C"); // server-truth title
  });

  it("maps the ⌘1–9 hint to each session's index in the FULL list", () => {
    expect(byId.get("session:b")?.hint).toBe("⌘2"); // full-list index 1
    expect(byId.get("session:c")?.hint).toBe("⌘3"); // full-list index 2
  });

  it("includes the split-pane actions when canSplit (default)", () => {
    const ids = built.map((c) => c.id);
    expect(ids).toContain("action:split-pane-right");
    expect(ids).toContain("action:split-pane-down");
  });

  it("omits the split-pane actions at the pane cap", () => {
    const capped = buildCommands({
      sessions,
      activeSessionId: "a",
      canSplit: false,
    });
    const ids = capped.map((c) => c.id);
    expect(ids).not.toContain("action:split-pane-right");
    expect(ids).not.toContain("action:split-pane-down");
  });

  it("includes the start-goal action only when a session is active", () => {
    const ids = built.map((c) => c.id);
    expect(ids).toContain("action:start-goal");
    const noActive = buildCommands({ sessions, activeSessionId: null });
    expect(noActive.map((c) => c.id)).not.toContain("action:start-goal");
  });

  it("shows ⌘9 for the 9th session and drops the hint past it", () => {
    const many = Array.from({ length: 11 }, (_, i) => session({ id: `s${i}` }));
    const builtMany = buildCommands({
      sessions: many,
      activeSessionId: null,
    });
    const byIdMany = new Map(builtMany.map((c) => [c.id, c] as const));
    expect(byIdMany.get("session:s8")?.hint).toBe("⌘9"); // index 8
    expect(byIdMany.get("session:s9")?.hint).toBeUndefined(); // index 9 → none
  });
});

describe("filterCommands", () => {
  const commands = buildCommands({
    sessions: [session({ id: "a" }), session({ id: "b", goal: "Write docs" })],
    activeSessionId: "a",
  });

  it("orders recently-run commands first on an empty query", () => {
    const ordered = filterCommands(commands, "", ["action:focus-composer"]);
    expect(ordered[0].id).toBe("action:focus-composer");
  });

  it("narrows to fuzzy matches and drops the rest", () => {
    const results = filterCommands(commands, "wrap", []);
    expect(results.map((c) => c.id)).toContain("action:toggle-wrap");
    expect(results.map((c) => c.id)).not.toContain("action:new-session");
  });
});

describe("buildSkillCommands", () => {
  const inactive: SkillInfo = {
    name: "rust-debugging",
    description: "Systematic Rust debugging",
    version: "0.1.0",
    keywords: ["rust", "debug"],
    active: false,
    score: 4,
  };
  const active: SkillInfo = { ...inactive, active: true, score: 0 };

  it("maps inactive skills to activate rows", () => {
    const [cmd] = buildSkillCommands([inactive]);
    expect(cmd.kind).toBe("activate-skill");
    expect(cmd.id).toBe("skill:activate:rust-debugging");
    expect(cmd.title).toBe("Activate rust-debugging");
    expect(cmd.hint).toBe("Activate");
  });

  it("maps active skills to deactivate rows", () => {
    const [cmd] = buildSkillCommands([active]);
    expect(cmd.kind).toBe("deactivate-skill");
    expect(cmd.id).toBe("skill:deactivate:rust-debugging");
    expect(cmd.hint).toBe("Active");
  });
});

describe("buildPhenotypeCommands", () => {
  const phenotypes: Phenotype[] = [
    { name: "default", skills: [], mcpServers: [], egress: "open" },
    {
      name: "rust",
      skills: ["rust-debugging"],
      mcpServers: [],
      egress: "open",
    },
  ];

  it("lists switch rows for non-active phenotypes", () => {
    const cmds = buildPhenotypeCommands({
      phenotypes,
      activePhenotype: phenotypes[0],
    });
    expect(cmds.map((c) => c.id)).toEqual(["pheno:rust"]);
    expect(cmds[0].kind).toBe("switch-phenotype");
  });
});

describe("buildMcpServerCommands", () => {
  const servers: McpServerStatus[] = [
    { id: "github", state: "running", toolCount: 8, restarts: 0 },
    { id: "playwright", state: "failed", toolCount: 0, restarts: 5 },
  ];

  it("builds one open-mcp-server row per server with the state as hint", () => {
    const cmds = buildMcpServerCommands(servers);
    expect(cmds.map((c) => c.id)).toEqual(["mcp:github", "mcp:playwright"]);
    expect(cmds[0]).toMatchObject({
      kind: "open-mcp-server",
      serverId: "github",
      title: "MCP: github",
      hint: "Running",
    });
    expect(cmds[1].hint).toBe("Failed");
  });

  it("fuzzy-matches servers by id via keywords", () => {
    const cmds = buildMcpServerCommands(servers);
    const hit = filterCommands(cmds, "github", []);
    expect(hit.map((c) => c.id)).toContain("mcp:github");
  });
});

describe("mergePaletteResults", () => {
  const staticCmds = buildCommands({
    sessions: [],
    activeSessionId: null,
  });
  const skillCmds = buildSkillCommands([
    {
      name: "create-pr",
      description: "Open a PR",
      version: "0.1.0",
      keywords: ["git"],
      active: false,
      score: 3,
    },
  ]);

  it("puts ranked skill hits first when the query is non-empty", () => {
    const merged = mergePaletteResults(staticCmds, skillCmds, "git", []);
    expect(merged[0].id).toBe("skill:activate:create-pr");
  });

  it("still fuzzy-filters static commands in the merge", () => {
    const merged = mergePaletteResults(staticCmds, skillCmds, "split", []);
    expect(merged.some((c) => c.id === "action:toggle-split")).toBe(true);
  });

  it("includes all skills on an empty query", () => {
    const merged = mergePaletteResults(staticCmds, skillCmds, "", []);
    expect(merged.some((c) => c.id === "skill:activate:create-pr")).toBe(true);
  });
});
