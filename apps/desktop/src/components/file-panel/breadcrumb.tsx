import { Fragment } from "react";
import { ChevronRight } from "@/components/ui/icon";
import { cn } from "@/lib/utils";

// Clickable path breadcrumb for the file viewer (#872). Segments are relative to
// the workspace root; clicking a directory segment calls `onNavigate` with that
// directory's rel-path (`""` for the root) so the tree can reveal it. The final
// segment is the file itself and is not clickable.

export function Breadcrumb({
  path,
  onNavigate,
}: {
  path: string;
  onNavigate: (dirPath: string) => void;
}) {
  const segments = path.split("/").filter(Boolean);
  return (
    <nav
      aria-label="File path"
      className="flex min-w-0 flex-wrap items-center gap-0.5 text-[12px] text-muted-foreground"
    >
      <button
        type="button"
        onClick={() => onNavigate("")}
        className="rounded px-1 py-0.5 hover:bg-foreground/10 hover:text-foreground"
      >
        workspace
      </button>
      {segments.map((seg, i) => {
        const isLast = i === segments.length - 1;
        const dirPath = segments.slice(0, i + 1).join("/");
        return (
          <Fragment key={dirPath}>
            <ChevronRight className="size-3 shrink-0 opacity-50" />
            {isLast ? (
              <span className="truncate px-1 py-0.5 font-medium text-foreground">
                {seg}
              </span>
            ) : (
              <button
                type="button"
                onClick={() => onNavigate(dirPath)}
                className={cn(
                  "truncate rounded px-1 py-0.5",
                  "hover:bg-foreground/10 hover:text-foreground",
                )}
              >
                {seg}
              </button>
            )}
          </Fragment>
        );
      })}
    </nav>
  );
}
