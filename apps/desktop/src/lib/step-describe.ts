// Human-readable one-line labels for tool steps (Issue #180). Derived frontend-side
// from tool + args — no model-authored description on the contract yet.

import { parseMcpToolName } from "@/lib/mcp";
import { parseTodo, todoSummary } from "@/lib/todo";
import type { ToolStep } from "@/store/chat";

/** Max length for a bash command's first line in the collapsed label. */
export const BASH_CMD_TRUNCATE = 60;

function strArg(args: unknown, key: string): string | null {
  if (!args || typeof args !== "object") return null;
  const v = (args as Record<string, unknown>)[key];
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

/** One-line description for a tool step header (Issue #180). */
export function describeStep(step: Pick<ToolStep, "tool" | "args">): string {
  const { tool, args } = step;

  switch (tool) {
    case "bash": {
      const command = strArg(args, "command");
      if (!command) return "Run command";
      const firstLine = command.split("\n")[0]?.trim() ?? command;
      const truncated =
        firstLine.length > BASH_CMD_TRUNCATE
          ? `${firstLine.slice(0, BASH_CMD_TRUNCATE)}…`
          : firstLine;
      return `Run \`${truncated}\``;
    }
    case "view": {
      const path = strArg(args, "path");
      return path ? `Read ${path}` : "Read file";
    }
    case "write": {
      const path = strArg(args, "path");
      return path ? `Write ${path}` : "Write file";
    }
    case "edit": {
      const path = strArg(args, "path");
      return path ? `Edit ${path}` : "Edit file";
    }
    case "grep": {
      const pattern = strArg(args, "pattern");
      return pattern ? `Search ${pattern}` : "Search";
    }
    case "glob": {
      const pattern = strArg(args, "pattern");
      return pattern ? `Find ${pattern}` : "Find files";
    }
    case "tree": {
      const path = strArg(args, "path");
      return path ? `List ${path}` : "List directory";
    }
    case "web_fetch": {
      const url = strArg(args, "url");
      return url ? `Fetch ${url}` : "Fetch URL";
    }
    case "todo": {
      const items = parseTodo(args);
      if (!items) return "Update plan";
      const { completed, total } = todoSummary(items);
      return `Update plan (${completed}/${total})`;
    }
    case "ask_user":
      return "Ask a question";
    default: {
      const mcp = parseMcpToolName(tool);
      if (mcp) return `${mcp.server}: ${mcp.tool}`;
      return tool;
    }
  }
}
