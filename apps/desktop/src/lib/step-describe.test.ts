import { describe, expect, it } from "vitest";

import { BASH_CMD_TRUNCATE, describeStep } from "@/lib/step-describe";

describe("describeStep", () => {
  it("bash: first line, truncated, backtick-wrapped", () => {
    const long = "a".repeat(BASH_CMD_TRUNCATE + 10);
    expect(describeStep({ tool: "bash", args: { command: "npm test" } })).toBe(
      "Run `npm test`",
    );
    expect(
      describeStep({
        tool: "bash",
        args: { command: "npm test\nnpm run lint" },
      }),
    ).toBe("Run `npm test`");
    expect(describeStep({ tool: "bash", args: { command: long } })).toBe(
      `Run \`${"a".repeat(BASH_CMD_TRUNCATE)}…\``,
    );
  });

  it("python: first line, truncated, prefixed", () => {
    const long = "x".repeat(BASH_CMD_TRUNCATE + 10);
    expect(describeStep({ tool: "python", args: { code: "print(1)" } })).toBe(
      "Run Python `print(1)`",
    );
    expect(
      describeStep({ tool: "python", args: { code: "import os\nprint(os)" } }),
    ).toBe("Run Python `import os`");
    expect(describeStep({ tool: "python", args: { code: long } })).toBe(
      `Run Python \`${"x".repeat(BASH_CMD_TRUNCATE)}…\``,
    );
    expect(describeStep({ tool: "python", args: {} })).toBe("Run Python");
  });

  it("process_manager: labels by action", () => {
    expect(
      describeStep({
        tool: "process_manager",
        args: { action: "start", command: "npm run dev" },
      }),
    ).toBe("Start `npm run dev`");
    const long = "x".repeat(BASH_CMD_TRUNCATE + 10);
    expect(
      describeStep({
        tool: "process_manager",
        args: { action: "start", command: long },
      }),
    ).toBe(`Start \`${"x".repeat(BASH_CMD_TRUNCATE)}…\``);
    expect(
      describeStep({
        tool: "process_manager",
        args: { action: "poll", process_id: 7 },
      }),
    ).toBe("Poll process #7");
    expect(
      describeStep({
        tool: "process_manager",
        args: { action: "stop", process_id: "7" },
      }),
    ).toBe("Stop process #7");
    expect(
      describeStep({ tool: "process_manager", args: { action: "list" } }),
    ).toBe("List processes");
    expect(describeStep({ tool: "process_manager", args: {} })).toBe(
      "Manage processes",
    );
    expect(
      describeStep({ tool: "process_manager", args: { action: "start" } }),
    ).toBe("Start process");
  });

  it("view / write / edit use path args", () => {
    expect(describeStep({ tool: "view", args: { path: "src/foo.ts" } })).toBe(
      "Read src/foo.ts",
    );
    expect(describeStep({ tool: "write", args: { path: "out.txt" } })).toBe(
      "Write out.txt",
    );
    expect(describeStep({ tool: "edit", args: { path: "README.md" } })).toBe(
      "Edit README.md",
    );
  });

  it("apply_patch: counts file sections", () => {
    const patch = [
      "*** Begin Patch",
      "*** Add File: a.txt",
      "+hi",
      "*** Update File: b.txt",
      "@@",
      "-x",
      "+y",
      "*** End Patch",
    ].join("\n");
    expect(describeStep({ tool: "apply_patch", args: { patch } })).toBe(
      "Patch 2 files",
    );
    const single = [
      "*** Begin Patch",
      "*** Delete File: gone.txt",
      "*** End Patch",
    ].join("\n");
    expect(describeStep({ tool: "apply_patch", args: { patch: single } })).toBe(
      "Patch 1 file",
    );
    expect(describeStep({ tool: "apply_patch", args: {} })).toBe("Apply patch");
  });

  it("grep / glob / tree use pattern or path", () => {
    expect(describeStep({ tool: "grep", args: { pattern: "FlowForge" } })).toBe(
      "Search FlowForge",
    );
    expect(describeStep({ tool: "glob", args: { pattern: "**/*.ts" } })).toBe(
      "Find **/*.ts",
    );
    expect(describeStep({ tool: "tree", args: { path: "." } })).toBe("List .");
  });

  it("web_fetch and ask_user", () => {
    expect(
      describeStep({ tool: "web_fetch", args: { url: "https://example.com" } }),
    ).toBe("Fetch https://example.com");
    expect(
      describeStep({ tool: "ask_user", args: { question: "Which?" } }),
    ).toBe("Ask a question");
  });

  it("agent: delegation label", () => {
    expect(
      describeStep({ tool: "agent", args: { task: "audit the foo module" } }),
    ).toBe("Delegate: audit the foo module");
    expect(
      describeStep({ tool: "agent", args: { task: "line one\nline two" } }),
    ).toBe("Delegate: line one");
    expect(describeStep({ tool: "agent", args: {} })).toBe(
      "Delegate to sub-agent",
    );
  });

  it("todo: done/total counts", () => {
    expect(
      describeStep({
        tool: "todo",
        args: {
          items: [
            { content: "a", status: "completed" },
            { content: "b", status: "pending" },
          ],
        },
      }),
    ).toBe("Update plan (1/2)");
  });

  it("MCP tools parse server:tool", () => {
    expect(
      describeStep({
        tool: "mcp__github__create_issue",
        args: {},
      }),
    ).toBe("github: create_issue");
  });

  it("falls back to the raw tool name", () => {
    expect(describeStep({ tool: "custom_tool", args: {} })).toBe("custom_tool");
  });
});
