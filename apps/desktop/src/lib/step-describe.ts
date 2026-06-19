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

/** A `process_id` arg, accepted as a number or numeric string. */
function procId(args: unknown): string | null {
  if (!args || typeof args !== "object") return null;
  const v = (args as Record<string, unknown>).process_id;
  if (typeof v === "number") return String(v);
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
    case "python": {
      const code = strArg(args, "code");
      if (!code) return "Run Python";
      const firstLine = code.split("\n")[0]?.trim() ?? code;
      const truncated =
        firstLine.length > BASH_CMD_TRUNCATE
          ? `${firstLine.slice(0, BASH_CMD_TRUNCATE)}…`
          : firstLine;
      return `Run Python \`${truncated}\``;
    }
    case "process_manager": {
      const action = strArg(args, "action");
      const pid = procId(args);
      switch (action) {
        case "start": {
          const command = strArg(args, "command");
          if (!command) return "Start process";
          const firstLine = command.split("\n")[0]?.trim() ?? command;
          const truncated =
            firstLine.length > BASH_CMD_TRUNCATE
              ? `${firstLine.slice(0, BASH_CMD_TRUNCATE)}…`
              : firstLine;
          return `Start \`${truncated}\``;
        }
        case "poll":
          return pid ? `Poll process #${pid}` : "Poll process";
        case "stop":
          return pid ? `Stop process #${pid}` : "Stop process";
        case "list":
          return "List processes";
        default:
          return "Manage processes";
      }
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
    case "apply_patch": {
      const patch = strArg(args, "patch");
      if (!patch) return "Apply patch";
      const files = (
        patch.match(/^\*\*\* (?:Add|Update|Delete) File: /gm) ?? []
      ).length;
      if (files === 0) return "Apply patch";
      return `Patch ${files} file${files === 1 ? "" : "s"}`;
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
