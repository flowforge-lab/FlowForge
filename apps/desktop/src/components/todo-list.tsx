import { CircleDot, Square, SquareCheck } from "lucide-react";
import { cn } from "@/lib/utils";
import type { TodoItem, TodoStatus } from "@/lib/todo";

const ICON: Record<TodoStatus, typeof Square> = {
  pending: Square,
  in_progress: CircleDot,
  completed: SquareCheck,
};

// Renders the `todo` planning tool's checklist (Issue #42) inside the tool step.
export function TodoList({ items }: { items: TodoItem[] }) {
  if (items.length === 0) {
    return <p className="text-muted-foreground/60">(empty checklist)</p>;
  }
  return (
    <ul className="flex flex-col gap-1">
      {items.map((item, i) => {
        const Icon = ICON[item.status];
        return (
          <li key={i} className="flex items-start gap-2">
            <Icon
              className={cn(
                "mt-0.5 size-3.5 shrink-0",
                item.status === "completed" && "text-emerald-500",
                item.status === "in_progress" && "text-amber-500",
                item.status === "pending" && "text-muted-foreground/50",
              )}
            />
            <span
              className={cn(
                "text-foreground/90",
                item.status === "completed" &&
                  "text-muted-foreground/60 line-through",
                item.status === "in_progress" && "text-foreground",
              )}
            >
              {item.content}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
