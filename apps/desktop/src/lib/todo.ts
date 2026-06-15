// Render model for the `todo` planning tool (Issue #42). The backend is stateless:
// the model passes the COMPLETE checklist as the tool call's args on every call
// (full-replace), so the frontend renders it straight from `ToolStep.args` — no
// new event or contract. Kept React-free so the parse is unit-testable.

export type TodoStatus = "pending" | "in_progress" | "completed";

export interface TodoItem {
  content: string;
  status: TodoStatus;
}

const STATUSES: readonly TodoStatus[] = ["pending", "in_progress", "completed"];

/**
 * Parse a `todo` tool call's args (`{ items: [{ content, status }] }`) into a
 * typed checklist. Returns `null` when the args aren't a todo payload at all (no
 * `items` array) so the caller can fall back to the generic step render; an empty
 * or all-invalid list yields `[]` (a valid empty checklist). Malformed individual
 * items are dropped defensively — the backend already validates on a real call.
 */
export function parseTodo(args: unknown): TodoItem[] | null {
  if (!args || typeof args !== "object") return null;
  const items = (args as { items?: unknown }).items;
  if (!Array.isArray(items)) return null;

  const result: TodoItem[] = [];
  for (const raw of items) {
    if (!raw || typeof raw !== "object") continue;
    const { content, status } = raw as { content?: unknown; status?: unknown };
    if (typeof content !== "string") continue;
    if (!STATUSES.includes(status as TodoStatus)) continue;
    result.push({ content, status: status as TodoStatus });
  }
  return result;
}

/** Completed-vs-total counts for the collapsed step header (e.g. "2/3"). */
export function todoSummary(items: TodoItem[]): {
  completed: number;
  total: number;
} {
  return {
    completed: items.filter((i) => i.status === "completed").length,
    total: items.length,
  };
}
