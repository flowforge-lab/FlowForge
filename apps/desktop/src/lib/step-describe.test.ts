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
