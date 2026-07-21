import { describe, expect, it } from "vitest";

import type { SkillInfo } from "@/bindings";
import type { CommandShortcut } from "@/store/command-shortcuts";
import {
  BUILTIN_SLASH_COMMANDS,
  buildSlashCommands,
  matchSlash,
  parseSlashQuery,
} from "@/lib/slash-command";

const skill = (over: Partial<SkillInfo>): SkillInfo => ({
  name: "grill-me",
  description: "Adversarial review of your reasoning",
  version: "1.0.0",
  keywords: [],
  active: false,
  score: 0,
  ...over,
});

const shortcut = (over: Partial<CommandShortcut>): CommandShortcut => ({
  id: "s1",
  name: "ship",
  message: "Open a PR and push the branch.",
  ...over,
});

describe("parseSlashQuery — the dropdown's open/closed gate", () => {
  it("opens on a bare slash with an empty query", () => {
    expect(parseSlashQuery("/")).toBe("");
  });

  it("returns the partial token while the name is being typed", () => {
    expect(parseSlashQuery("/gr")).toBe("gr");
    expect(parseSlashQuery("/grill-me")).toBe("grill-me");
  });

  it("tolerates leading whitespace, like parseGoalCommand does", () => {
    expect(parseSlashQuery("  /go")).toBe("go");
  });

  it("closes as soon as a space ends the command name", () => {
    // This is what keeps `/goal <objective>` working: the list is gone by the
    // time the user types their objective, so Enter falls through to submit().
    expect(parseSlashQuery("/goal ")).toBeNull();
    expect(parseSlashQuery("/goal ship the thing")).toBeNull();
  });

  it("ignores a slash that isn't leading", () => {
    expect(parseSlashQuery("see a/b")).toBeNull();
    expect(parseSlashQuery("hello /goal")).toBeNull();
  });

  it("returns null for ordinary text and an empty composer", () => {
    expect(parseSlashQuery("")).toBeNull();
    expect(parseSlashQuery("what is this")).toBeNull();
  });

  it("treats a newline as whitespace (a multi-line draft isn't a command)", () => {
    expect(parseSlashQuery("/goal\nmore")).toBeNull();
  });

  it("closes on ANY whitespace, not just a single space (#1043 review)", () => {
    // The rule is `/\s/`, and every form of it is load-bearing: whichever
    // whitespace ends the name, the list must close so Enter falls back to
    // submit() and `/goal <objective>` reaches parseGoalCommand untouched. Pinned
    // explicitly so a refactor of parseSlashQuery can't narrow this to `" "`.
    expect(parseSlashQuery("/goal\tship it")).toBeNull();
    expect(parseSlashQuery("/goal\t")).toBeNull();
    expect(parseSlashQuery("/goal  ship it")).toBeNull();
    expect(parseSlashQuery("/goal   ")).toBeNull();
    // Leading whitespace is still tolerated — it's trimmed before the check, so
    // only whitespace INSIDE the token closes the list.
    expect(parseSlashQuery("\t/goal")).toBe("goal");
    expect(parseSlashQuery("  /goal\tship it")).toBeNull();
  });
});

describe("buildSlashCommands — merging the three sources", () => {
  it("lists builtins, skills, and shortcuts, builtins first", () => {
    const cmds = buildSlashCommands({
      skills: [skill({}), skill({ name: "tdd", description: "Test first" })],
      shortcuts: [shortcut({})],
    });
    expect(cmds.map((c) => c.kind)).toEqual([
      "builtin",
      "skill",
      "skill",
      "shortcut",
    ]);
    expect(cmds.map((c) => c.name)).toEqual([
      "goal",
      "grill-me",
      "tdd",
      "ship",
    ]);
  });

  it("carries the shortcut message as the dispatch payload", () => {
    const [cmd] = buildSlashCommands({
      skills: [],
      shortcuts: [shortcut({ message: "ship it" })],
    }).filter((c) => c.kind === "shortcut");
    expect(cmd.payload).toBe("ship it");
  });

  it("marks an already-active skill so accepting can skip the IPC", () => {
    const [s] = buildSlashCommands({
      skills: [skill({ active: true })],
      shortcuts: [],
    }).filter((c) => c.kind === "skill");
    expect(s.active).toBe(true);
    expect(s.hint).toBe("Active");
  });

  it("flags skills as inapplicable when the session is bound to a phenotype", () => {
    // `turn_active_skills` resolves a phenotype-bound session from the phenotype
    // and ignores the global active set — activating would be a silent no-op.
    const [s] = buildSlashCommands({
      skills: [skill({})],
      shortcuts: [],
      sessionPhenotype: "reviewer",
    }).filter((c) => c.kind === "skill");
    expect(s.hint).toBe("Won't apply");
  });

  it("still offers the builtins with no skills or shortcuts installed", () => {
    expect(buildSlashCommands({ skills: [], shortcuts: [] })).toEqual([
      ...BUILTIN_SLASH_COMMANDS,
    ]);
  });
});

describe("matchSlash — ranking", () => {
  const cmds = buildSlashCommands({
    skills: [
      skill({}),
      skill({ name: "tdd", description: "Write the test first" }),
    ],
    shortcuts: [shortcut({})],
  });

  it("lists everything in registry order for a bare slash", () => {
    expect(matchSlash(cmds, "").map((c) => c.name)).toEqual([
      "goal",
      "grill-me",
      "tdd",
      "ship",
    ]);
  });

  it("filters to fuzzy matches, best first", () => {
    const names = matchSlash(cmds, "gr").map((c) => c.name);
    expect(names[0]).toBe("grill-me");
    // Subsequence matching is deliberately loose (same as ⌘K), so a weaker hit
    // deeper in another row's text may still trail — it just must not lead.
    expect(names).not.toContain("ship");
  });

  it("ranks a name hit above a description-only hit", () => {
    const withDecoy = buildSlashCommands({
      skills: [
        skill({ name: "review", description: "grill the reasoning hard" }),
        skill({ name: "grill-me", description: "adversarial" }),
      ],
      shortcuts: [],
    });
    expect(matchSlash(withDecoy, "grill")[0].name).toBe("grill-me");
  });

  it("returns an empty list when nothing matches", () => {
    expect(matchSlash(cmds, "zzzz")).toEqual([]);
  });

  it("does not mutate the registry it was given", () => {
    const before = [...cmds];
    matchSlash(cmds, "g");
    expect(cmds).toEqual(before);
  });
});
