import { useEffect } from "react";
import { Cpu, Eye, FileText, Globe, X } from "@/components/ui/icon";
import { useObserversStore } from "@/store/observers";
import type { ObserverInfo, ObserverKind } from "@/bindings";

// Active background observers (#1038 FE, epic #954 M2). A self-hiding strip in
// each session pane, mounted alongside the process/notebook/goal panels. It
// lists the session's live observers — the file/http/process watchers the agent
// attached — with a kind icon, the watched target, a coarse kind hint, and a
// `[×]` to stop one.
//
// Command + event hybrid (unlike the push-only process panel): the set is read
// with `list_observers` on mount and re-read whenever `observer:changed` fires
// (wired in `lib/events.ts`) — so it live-updates as observers start, stop, and
// fire without a manual refresh.
//
// Self-hides when the session has no active observers (store entry absent or
// empty), so a session that never attaches one shows nothing at all.

export function ObserverPanel({ sessionId }: { sessionId: string }) {
  const observers = useObserversStore((s) => s.bySession[sessionId]);

  // Command-based, so load once on mount (and on session change). Later updates
  // arrive via the `observer:changed` subscription in `lib/events.ts`.
  useEffect(() => {
    void useObserversStore.getState().load(sessionId);
  }, [sessionId]);

  if (!observers || observers.length === 0) return null;

  return (
    <div className="flex shrink-0 flex-col border-b bg-card/40">
      <div className="flex items-center gap-1.5 px-2.5 py-1.5">
        <Eye className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="text-[11px] font-medium text-foreground">
          Observers ({observers.length})
        </span>
      </div>
      {observers.map((o) => (
        <ObserverRow key={o.id} observer={o} sessionId={sessionId} />
      ))}
    </div>
  );
}

function ObserverRow({
  observer,
  sessionId,
}: {
  observer: ObserverInfo;
  sessionId: string;
}) {
  return (
    <div className="flex items-center gap-1.5 border-t px-2.5 py-1.5">
      <KindIcon kind={observer.kind} />
      <span className="min-w-0 flex-1 truncate text-[11px] text-foreground">
        <span title={observer.target}>{observer.target}</span>
        <span className="ml-1.5 text-muted-foreground">
          {kindHint(observer.kind)}
        </span>
      </span>
      <button
        type="button"
        onClick={() =>
          void useObserversStore.getState().stop(observer.id, sessionId)
        }
        aria-label={`Stop observer ${observer.label}`}
        title="Stop observer"
        className="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:bg-foreground/5 hover:text-foreground"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}

function KindIcon({ kind }: { kind: ObserverKind }) {
  const className = "size-3.5 shrink-0 text-muted-foreground";
  switch (kind) {
    case "file":
      return <FileText className={className} />;
    case "http":
      return <Globe className={className} />;
    case "process":
      return <Cpu className={className} />;
  }
}

// Coarse, kind-derived hint. `ObserverInfo` carries no filter/interval (those
// live on the internal `ObserverSpec`), so M2 shows a kind label rather than an
// exact cadence like "polling 30s" — see #1038.
function kindHint(kind: ObserverKind): string {
  switch (kind) {
    case "file":
      return "file changes";
    case "http":
      return "polling";
    case "process":
      return "process";
  }
}
