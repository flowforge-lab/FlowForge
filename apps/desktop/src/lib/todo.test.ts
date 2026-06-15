import { describe, it, expect } from "vitest";
import { parseTodo, todoSummary, type TodoItem } from "@/lib/todo";

describe("parseTodo", () => {
  it("parses a well-formed checklist, preserving order", () => {
    const items = parseTodo({
      items: [
        { content: "design", status: "completed" },
        { content: "build", status: "in_progress" },
        { content: "ship", status: "pending" },
      ],
    });
    expect(items).toEqual([
      { content: "design", status: "completed" },
      { content: "build", status: "in_progress" },
      { content: "ship", status: "pending" },
    ]);
  });

  it("returns [] for an empty checklist", () => {
    expect(parseTodo({ items: [] })).toEqual([]);
  });

  it("returns null when args aren't a todo payload", () => {
    expect(parseTodo(null)).toBeNull();
    expect(parseTodo("nope")).toBeNull();
    expect(parseTodo({})).toBeNull(); // no items array
    expect(parseTodo({ items: "x" })).toBeNull();
  });

  it("drops items with an invalid status or missing content", () => {
    const items = parseTodo({
      items: [
        { content: "ok", status: "pending" },
        { content: "bad status", status: "done" },
        { status: "completed" }, // no content
        { content: 42, status: "pending" }, // non-string content
      ],
    });
    expect(items).toEqual([{ content: "ok", status: "pending" }]);
  });
});

describe("todoSummary", () => {
  it("counts completed vs total", () => {
    const items: TodoItem[] = [
      { content: "a", status: "completed" },
      { content: "b", status: "in_progress" },
      { content: "c", status: "completed" },
    ];
    expect(todoSummary(items)).toEqual({ completed: 2, total: 3 });
  });

  it("is zeroes for an empty list", () => {
    expect(todoSummary([])).toEqual({ completed: 0, total: 0 });
  });
});
