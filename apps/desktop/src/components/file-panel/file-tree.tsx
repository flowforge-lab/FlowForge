import {
  ChevronDown,
  ChevronRight,
  FileText,
  Folder,
  FolderOpen,
} from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useFilePanelStore } from "@/store/file-panel";
import type { DirEntry } from "@/bindings";

// Recursive, lazily-loaded workspace file tree (#872). Expanding a directory
// fetches its listing via the store (cached thereafter); clicking a file selects
// it into the viewer. Rendering is driven entirely by store state so persisted
// expansion + selection re-hydrate on reload.

/** Join a parent rel-path and a child name into a workspace-relative path. */
function joinPath(parent: string, name: string): string {
  return parent ? `${parent}/${name}` : name;
}

function Row({
  depth,
  onClick,
  selected,
  children,
}: {
  depth: number;
  onClick: () => void;
  selected?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      style={{ paddingLeft: 8 + depth * 12 }}
      className={cn(
        "flex w-full items-center gap-1.5 py-1 pr-2 text-left text-[12.5px] transition-colors",
        selected
          ? "bg-primary/15 text-foreground"
          : "text-foreground/85 hover:bg-foreground/10",
      )}
    >
      {children}
    </button>
  );
}

function TreeNode({
  sessionId,
  entry,
  path,
  depth,
}: {
  sessionId: string;
  entry: DirEntry;
  path: string;
  depth: number;
}) {
  const expanded = useFilePanelStore(
    (s) => s.bySession[sessionId]?.expanded.has(path) ?? false,
  );
  const selected = useFilePanelStore(
    (s) => s.bySession[sessionId]?.selectedPath === path,
  );
  const children = useFilePanelStore((s) => s.bySession[sessionId]?.tree[path]);
  const dirError = useFilePanelStore(
    (s) => s.bySession[sessionId]?.dirError[path],
  );
  const toggleExpand = useFilePanelStore((s) => s.toggleExpand);
  const selectFile = useFilePanelStore((s) => s.selectFile);

  if (entry.isDir) {
    return (
      <>
        <Row depth={depth} onClick={() => void toggleExpand(sessionId, path)}>
          {expanded ? (
            <ChevronDown className="size-3.5 shrink-0 opacity-60" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0 opacity-60" />
          )}
          {expanded ? (
            <FolderOpen className="size-3.5 shrink-0 text-sky-500" />
          ) : (
            <Folder className="size-3.5 shrink-0 text-sky-500" />
          )}
          <span className="truncate">{entry.name}</span>
        </Row>
        {expanded && (
          <>
            {dirError ? (
              <p
                style={{ paddingLeft: 8 + (depth + 1) * 12 }}
                className="py-1 pr-2 text-[12px] text-destructive"
              >
                {dirError}
              </p>
            ) : (
              <TreeLevel
                sessionId={sessionId}
                dir={path}
                depth={depth + 1}
                entries={children}
              />
            )}
          </>
        )}
      </>
    );
  }

  return (
    <Row
      depth={depth}
      selected={selected}
      onClick={() => void selectFile(sessionId, path)}
    >
      {/* Spacer aligning file icons under the folder chevron column. */}
      <span className="size-3.5 shrink-0" />
      <FileText className="size-3.5 shrink-0 text-muted-foreground" />
      <span className="truncate">{entry.name}</span>
    </Row>
  );
}

function TreeLevel({
  sessionId,
  dir,
  depth,
  entries,
}: {
  sessionId: string;
  dir: string;
  depth: number;
  entries: DirEntry[] | undefined;
}) {
  if (entries === undefined) {
    return (
      <p
        style={{ paddingLeft: 8 + depth * 12 }}
        className="py-1 pr-2 text-[12px] text-muted-foreground/70"
      >
        Loading…
      </p>
    );
  }
  if (entries.length === 0) {
    return (
      <p
        style={{ paddingLeft: 8 + depth * 12 }}
        className="py-1 pr-2 text-[12px] italic text-muted-foreground/60"
      >
        empty
      </p>
    );
  }
  return (
    <>
      {entries.map((e) => (
        <TreeNode
          key={joinPath(dir, e.name)}
          sessionId={sessionId}
          entry={e}
          path={joinPath(dir, e.name)}
          depth={depth}
        />
      ))}
    </>
  );
}

export function FileTree({ sessionId }: { sessionId: string }) {
  const root = useFilePanelStore((s) => s.bySession[sessionId]?.tree[""]);
  const rootError = useFilePanelStore(
    (s) => s.bySession[sessionId]?.dirError[""],
  );

  if (rootError) {
    return (
      <p className="px-3 py-2 text-[12px] text-destructive">{rootError}</p>
    );
  }
  return (
    <div className="py-1">
      <TreeLevel sessionId={sessionId} dir="" depth={0} entries={root} />
    </div>
  );
}
