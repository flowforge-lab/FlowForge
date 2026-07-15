import { useEffect } from "react";
import { useChatStore } from "@/store/chat";
import { useFilePanelStore } from "@/store/file-panel";
import { FileTree } from "./file-tree";
import { FileViewer } from "./file-viewer";

// Files panel shell (#872): a workspace file tree on the left, the file viewer
// on the right. Rendered inside the split panel via the `{ kind: "files" }`
// surface. Points the store at the active session on mount / session change,
// which loads the root listing and re-hydrates persisted expansion + selection.

export function FilePanel() {
  const sessionId = useChatStore((s) => s.activeSessionId);
  const syncSession = useFilePanelStore((s) => s.syncSession);

  useEffect(() => {
    if (sessionId) void syncSession(sessionId);
  }, [sessionId, syncSession]);

  if (!sessionId) {
    return (
      <div className="flex h-full w-full items-center justify-center px-6 text-center text-[13px] text-muted-foreground/70">
        No active session.
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-w-0">
      <div className="w-52 shrink-0 overflow-auto border-r bg-card/50">
        <FileTree />
      </div>
      <div className="min-w-0 flex-1">
        <FileViewer />
      </div>
    </div>
  );
}
