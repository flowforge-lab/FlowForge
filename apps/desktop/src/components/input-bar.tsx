import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowUp,
  Check,
  ChevronsUpDown,
  EyeOff,
  FileText,
  Folder,
  GitBranch,
  Paperclip,
  PencilLine,
  Search,
  Square,
  X,
} from "@/components/ui/icon";
import { Popover as PopoverPrimitive } from "radix-ui";
import { Button } from "@/components/ui/button";
import { ModelChip } from "@/components/model-chip";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { Attachment } from "@/bindings";
import { attachmentKindFor, fileToAttachment } from "@/lib/attachments";
import { cn } from "@/lib/utils";
import {
  useModelConfigStore,
  activeConnection,
  isLocalKind,
} from "@/store/model-config";
import { formatBytes } from "@/lib/memory-view";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import { useSessionModeStore, MODE_ORDER } from "@/store/session-mode";
import { useSessionModelStore } from "@/store/session-model";
import { MODE_META } from "@/lib/mode";

// A local model server (candle-vllm, Ollama, …) clocks its GPU down when idle,
// so the first token after a pause crawls while the device ramps back up. We
// nudge it (`ipc.warmup`) while the user interacts with the composer — on focus
// and as they type — so the device is at full clock by the time they hit send
// and the first real token streams immediately.
//
// Measured on Apple Silicon: warmth decays ~7-10s after activity, so the
// throttle sits just under that window. When already warm a nudge is ~0.4s of
// GPU; cold, it absorbs the ramp the real turn would otherwise pay.
const WARMUP_THROTTLE_MS = 5_000;

// `sessionId` scopes the composer to one session so split panes (#148) each keep
// an independent draft and Stop/send target their own session. Defaults to the
// active session for the single-pane layout.
export function InputBar({
  sessionId,
  focused = true,
}: { sessionId?: string; focused?: boolean } = {}) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Read inside the focus effects without making them depend on `focused`: only
  // the focused pane (#148) should grab the composer, but merely focusing a pane
  // (e.g. selecting transcript text) must not yank the caret into its textarea.
  // Synced in an effect (not during render) so the focus effects below — which
  // run later in declaration order — always see the current value.
  const focusedRef = useRef(focused);
  useEffect(() => {
    focusedRef.current = focused;
  });

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const targetSessionId = sessionId ?? activeSessionId ?? undefined;

  // Composer text lives in a per-session store so "edit & resend" (Issue #18) can
  // prefill it from a message row without prop-drilling, and each pane (#148)
  // keeps its own draft.
  const value = useComposerStore((s) =>
    targetSessionId ? (s.textBySession[targetSessionId] ?? "") : "",
  );
  const setTextFor = useComposerStore((s) => s.setText);
  const focusNonce = useComposerStore((s) =>
    targetSessionId ? (s.focusNonceBySession[targetSessionId] ?? 0) : 0,
  );
  const rejectNonce = useComposerStore((s) =>
    targetSessionId ? (s.rejectNonceBySession[targetSessionId] ?? 0) : 0,
  );
  // In-place edit mode (#463): the user message id being edited in this pane, or
  // undefined when composing fresh. Routes submit to `editMessage` and shows a banner.
  const editingMessageId = useComposerStore((s) =>
    targetSessionId ? s.editingBySession[targetSessionId] : undefined,
  );
  const cancelEdit = useComposerStore((s) => s.cancelEdit);
  const setText = useCallback(
    (text: string) => {
      if (targetSessionId) setTextFor(targetSessionId, text);
    },
    [targetSessionId, setTextFor],
  );

  const streaming = useChatStore((s) =>
    targetSessionId ? Boolean(s.streamingBySession[targetSessionId]) : false,
  );
  // The gap between hitting send and the first streamed token: the turn is
  // in flight on the backend but nothing renders yet. Derived from the
  // existing timing/streaming maps (turn started, no tokens) so the send
  // button offers Stop (the turn is cancellable here) while the transcript
  // shows a "thinking" indicator.
  const pending = useChatStore((s) =>
    targetSessionId
      ? Boolean(s.turnStartBySession[targetSessionId]) &&
        !s.streamingBySession[targetSessionId]
      : false,
  );
  const send = useChatStore((s) => s.send);
  const editMessage = useChatStore((s) => s.editMessage);
  const cancelTurn = useChatStore((s) => s.cancelTurn);
  const sendMessageKey = usePrefsStore((s) => s.sendMessageKey);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [dragOver, setDragOver] = useState(false);

  // Capability gates (#342/#504, RFC 0005 §11.3): the resolved model for THIS pane
  // may accept images, documents, both, or neither. Caps are derived backend-side
  // from the resolved `(kind, model)` and carried on `ResolvedModel` (the model chip
  // loads it per session), so a per-session model override gates the composer by the
  // model it actually runs — not the global active connection. Fail OPEN when unknown
  // (not loaded yet / no session) so the composer is never falsely blocked. Vision
  // and documents gate independently.
  const resolved = useSessionModelStore((s) =>
    targetSessionId ? s.resolvedBySession[targetSessionId] : undefined,
  );
  const supportsVision = resolved?.supportsVision;
  const supportsDocuments = resolved?.supportsDocuments;
  const visionGated = supportsVision === false;
  const docGated = supportsDocuments === false;
  // The attach button is only fully disabled when neither kind is allowed.
  const attachGated = visionGated && docGated;
  // Capability-aware affordance copy: name only the kinds the model can take.
  const attachLabel =
    !visionGated && !docGated
      ? "Attach image or document"
      : visionGated
        ? "Attach document"
        : "Attach image";

  // Stage a file only if its kind is one the active model accepts (#504): images
  // need vision, documents need document support. Unsupported file types are
  // dropped. Mirrors the backend `messages_for_wire` strip so the composer never
  // stages something the provider would silently discard.
  const canStage = useCallback(
    (file: File) => {
      const kind = attachmentKindFor(file);
      if (kind === "image") return !visionGated;
      if (kind === "document") return !docGated;
      return false;
    },
    [visionGated, docGated],
  );

  // Resolved mode for this pane's session — drives the Plan-aware placeholder
  // (#267, RFC 0011 §8). Switching modes is done via the pill dropdown (#344).
  const defaultMode = usePrefsStore((s) => s.defaultMode);
  const explicitMode = useSessionModeStore((s) =>
    targetSessionId ? s.modeBySession[targetSessionId] : undefined,
  );
  const mode = explicitMode ?? defaultMode;

  const autoGrow = useCallback((el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, []);

  // --- Attach affordances (#339) ---

  const handlePaste = useCallback(
    (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
      // Attach-gated (#342/#504): let the paste fall through as text; stage nothing.
      if (attachGated) return;
      const items = Array.from(e.clipboardData.items);
      for (const item of items) {
        if (item.kind !== "file") continue;
        const file = item.getAsFile();
        if (!file || !canStage(file)) continue;
        e.preventDefault();
        fileToAttachment(file).then((att) =>
          setAttachments((prev) => [...prev, att]),
        );
      }
    },
    [attachGated, canStage],
  );

  const handleDragOver = useCallback(
    (e: React.DragEvent) => {
      if (attachGated) return;
      e.preventDefault();
      e.stopPropagation();
      setDragOver(true);
    },
    [attachGated],
  );

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // Only clear when leaving the container entirely, not when crossing a child.
    if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragOver(false);
  }, []);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setDragOver(false);
      if (attachGated) return;
      const files = Array.from(e.dataTransfer.files);
      for (const file of files) {
        if (!canStage(file)) continue;
        fileToAttachment(file).then((att) =>
          setAttachments((prev) => [...prev, att]),
        );
      }
    },
    [attachGated, canStage],
  );

  function handleFilePick(e: React.ChangeEvent<HTMLInputElement>) {
    const files = e.target.files;
    if (!files || attachGated) return;
    for (const file of Array.from(files)) {
      if (!canStage(file)) continue;
      fileToAttachment(file).then((att) =>
        setAttachments((prev) => [...prev, att]),
      );
    }
    // Reset so the same file can be re-selected.
    e.target.value = "";
  }

  // Warmup only helps local backends, and firing it against a hosted endpoint
  // would bill a request on every focus (#61). Also honor the per-connection
  // toggle (off e.g. on laptop battery). The backend gates too (defense in depth).
  const warmupAllowed = useModelConfigStore((s) => {
    const conn = activeConnection(s.registry);
    return conn ? isLocalKind(conn.kind) && conn.warmupEnabled : false;
  });

  // Throttled, fire-and-forget server warmup (see note at top of file).
  const lastWarmupRef = useRef(0);
  const warmup = useCallback(() => {
    if (!warmupAllowed) return;
    const now = Date.now();
    if (now - lastWarmupRef.current < WARMUP_THROTTLE_MS) return;
    lastWarmupRef.current = now;
    void ipc.warmup().catch(() => {});
  }, [warmupAllowed]);

  // Keyboard-native: focus follows the (pane's) session — but only for the
  // focused pane, so background panes don't steal focus on mount.
  useEffect(() => {
    if (focusedRef.current) textareaRef.current?.focus();
  }, [targetSessionId]);

  // Edit & resend prefills the text and bumps focusNonce; focus, grow, and drop
  // the caret at the end here — all DOM, no state set in the effect.
  useEffect(() => {
    const el = textareaRef.current;
    if (!el || !focusedRef.current) return;
    el.focus();
    autoGrow(el);
    el.setSelectionRange(el.value.length, el.value.length);
  }, [focusNonce, autoGrow]);

  // A refused prefill (#48) kept an in-progress draft instead of clobbering it —
  // shake the composer and refocus so the action isn't silently ignored. DOM
  // only, no state set in the effect. (rejectNonce starts at 0; skip that.)
  useEffect(() => {
    if (rejectNonce === 0 || !focusedRef.current) return;
    boxRef.current?.animate(
      [
        { transform: "translateX(0)" },
        { transform: "translateX(-4px)" },
        { transform: "translateX(4px)" },
        { transform: "translateX(-3px)" },
        { transform: "translateX(0)" },
      ],
      { duration: 350, easing: "ease-in-out" },
    );
    textareaRef.current?.focus();
  }, [rejectNonce]);

  function submit() {
    const content = value.trim();
    if (
      (!content && attachments.length === 0) ||
      // A fresh send waits for the turn to settle, but an in-place edit (#463) may
      // be submitted mid-turn — the backend cancels the running turn and re-runs.
      ((streaming || pending) && !editingMessageId) ||
      !targetSessionId
    )
      return;
    const attach = attachments;
    setAttachments([]);
    // Collapse the box back to one line (it may have grown for a resend draft).
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    if (editingMessageId) {
      // In-place edit (#463): replace + re-run from the edited message. cancelEdit
      // clears both the editing binding and the composer text.
      cancelEdit(targetSessionId);
      void editMessage(targetSessionId, editingMessageId, content, attach);
    } else {
      setText("");
      void send(content, targetSessionId, attach);
    }
  }

  return (
    <div className="px-4 pb-4 pt-2">
      <div className="mx-auto flex max-w-4xl flex-col gap-2">
        {/* Primary composer card: textarea + mode/model/attach + send/stop. The
            workspace + branch live in a separate bar below (#606). */}
        <div
          ref={boxRef}
          className={cn(
            "rounded-xl border bg-card p-1.5 shadow-sm transition-all focus-within:border-ring focus-within:shadow-md focus-within:ring-2 focus-within:ring-ring/25",
            dragOver && "border-primary ring-2 ring-primary/30",
          )}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
        >
          {/* In-place edit banner (#463): submitting replaces the original message
              and re-runs from it; Cancel/Escape exits without mutating history. */}
          {editingMessageId && targetSessionId ? (
            <div className="mb-1 flex items-center gap-1.5 rounded-lg bg-muted/50 px-2.5 py-1.5 text-[11px] text-muted-foreground">
              <PencilLine className="size-3 shrink-0" />
              <span className="flex-1">
                Editing message — submitting replaces it and re-runs.
              </span>
              <button
                type="button"
                onClick={() => cancelEdit(targetSessionId)}
                className="flex items-center gap-0.5 rounded px-1 py-0.5 font-medium hover:bg-foreground/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
              >
                <X className="size-3" /> Cancel
              </button>
            </div>
          ) : null}
          <textarea
            ref={textareaRef}
            data-composer
            data-pane-focused={focused ? "" : undefined}
            value={value}
            rows={1}
            placeholder={
              mode === "plan"
                ? "Plan mode — ask the agent to read and propose…"
                : "Send a message..."
            }
            className="max-h-40 min-h-20 w-full resize-none bg-transparent px-2 py-1.5 text-[13px] leading-relaxed placeholder:text-muted-foreground/50 focus-visible:outline-none"
            onFocus={warmup}
            onPaste={handlePaste}
            onChange={(e) => {
              warmup();
              setText(e.currentTarget.value);
              autoGrow(e.currentTarget);
            }}
            onKeyDown={(e) => {
              // Escape exits in-place edit mode without mutating the transcript.
              // Stop propagation so the shell's global Esc (cancel active turn,
              // app-shell.tsx) doesn't also fire — abandoning the edit must not
              // also kill an in-flight turn.
              if (e.key === "Escape" && editingMessageId && targetSessionId) {
                e.preventDefault();
                e.stopPropagation();
                cancelEdit(targetSessionId);
                return;
              }
              if (e.key !== "Enter") return;
              // Enter mode: plain Enter sends (Shift+Enter = new line, unchanged).
              // Ctrl+Enter mode: Ctrl/⌘+Enter sends; any other Enter is a new line.
              const sends =
                sendMessageKey === "ctrlEnter"
                  ? e.metaKey || e.ctrlKey
                  : !e.shiftKey;
              if (sends) {
                e.preventDefault();
                submit();
              }
            }}
          />

          {/* Staged attachments (#340): one removable chip/thumbnail per file,
              between the textarea and the toolbar so it reads as part of the draft. */}
          {attachments.length > 0 ? (
            <AttachmentChips
              attachments={attachments}
              onRemove={(idx) =>
                setAttachments((prev) => prev.filter((_, i) => i !== idx))
              }
            />
          ) : null}

          {/* Bottom toolbar inside the composer: mode/model/attach (left) and
              Send/Stop (right), so the controls read as one input box. */}
          <div className="flex items-center justify-between gap-2 border-t border-border/40 px-1.5 pb-1 pt-1.5">
            <div className="flex min-w-0 items-center gap-1.5">
              {targetSessionId ? (
                <>
                  <input
                    ref={fileInputRef}
                    type="file"
                    accept="image/*,.csv,.doc,.docx,.html,.htm,.md,.markdown,.pdf,.txt,.xls,.xlsx,.json,application/pdf,application/json"
                    multiple
                    className="hidden"
                    onChange={handleFilePick}
                  />
                  <ModePill sessionId={targetSessionId} />
                  <ModelChip sessionId={targetSessionId} />
                  {attachGated ? (
                    // Capability gate (#342/#504): the active model accepts neither
                    // images nor documents, so the attach button is disabled + badged
                    // with an EyeOff marker and a tooltip explaining why. The disabled
                    // <button> won't fire pointer events, so the tooltip hangs off a
                    // <span> wrapper.
                    <TooltipProvider delayDuration={150}>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="inline-flex">
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon-xs"
                              disabled
                              className="relative shrink-0 text-muted-foreground"
                              aria-label="Attach (unavailable: this model can't accept attachments)"
                            >
                              <Paperclip className="size-3.5" />
                              <EyeOff className="absolute -bottom-0.5 -right-0.5 size-2.5 rounded-full bg-card" />
                            </Button>
                          </span>
                        </TooltipTrigger>
                        <TooltipContent className="max-w-56">
                          This model can&apos;t accept attachments — switch to a
                          model that supports images or documents.
                        </TooltipContent>
                      </Tooltip>
                    </TooltipProvider>
                  ) : (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      className="shrink-0 text-muted-foreground hover:text-foreground"
                      onClick={() => fileInputRef.current?.click()}
                      title={attachLabel}
                      aria-label={attachLabel}
                    >
                      <Paperclip className="size-3.5" />
                    </Button>
                  )}
                </>
              ) : (
                <span />
              )}
            </div>
            {(streaming || pending) && !editingMessageId ? (
              <Button
                variant="outline"
                size="icon"
                className="size-8 shrink-0 rounded-lg"
                onClick={() =>
                  targetSessionId && void cancelTurn(targetSessionId)
                }
                title="Stop (Esc)"
              >
                <Square className="size-3.5" />
              </Button>
            ) : (
              <Button
                size="icon"
                className="size-8 shrink-0 rounded-lg"
                disabled={
                  (!value.trim() && attachments.length === 0) ||
                  !targetSessionId
                }
                onClick={submit}
                title={
                  sendMessageKey === "ctrlEnter"
                    ? "Send (⌘/Ctrl+Enter)"
                    : "Send (Enter)"
                }
              >
                <ArrowUp className="size-4" />
              </Button>
            )}
          </div>
        </div>

        {/* Separate workspace bar (#606): session context — workspace + git
            branch — sits below the card as a distinct, compact row rather than
            another control mixed into the toolbar. */}
        {targetSessionId ? (
          <div
            data-testid="workspace-bar"
            className="flex min-w-0 items-center px-1 text-xs"
          >
            <WorkspaceSelector sessionId={targetSessionId} />
          </div>
        ) : null}
      </div>
    </div>
  );
}

// Staged-attachment strip (#340). Renders one removable chip per file the user has
// staged in the composer: a thumbnail for images, a document icon otherwise, plus the
// file name, humanized size, and type. Pure view over the composer's local state.
function AttachmentChips({
  attachments,
  onRemove,
}: {
  attachments: Attachment[];
  onRemove: (index: number) => void;
}) {
  return (
    <div className="flex flex-wrap gap-1.5 px-1.5 pt-1.5">
      {attachments.map((att, idx) => (
        <AttachmentChip
          // No stable id on a staged attachment; index is fine for an
          // append/remove-only list that never reorders.
          key={idx}
          attachment={att}
          onRemove={() => onRemove(idx)}
        />
      ))}
    </div>
  );
}

function AttachmentChip({
  attachment,
  onRemove,
}: {
  attachment: Attachment;
  onRemove: () => void;
}) {
  const { kind, mediaType, source, name, bytes } = attachment;
  const isImage = kind === "image";
  // Short, human type label from the IANA media type: "image/png" -> "PNG".
  const typeLabel =
    mediaType.split("/")[1]?.toUpperCase() ?? kind.toUpperCase();
  const label = name ?? typeLabel;

  // Inline (base64) previews resolve synchronously; a path reference is resolved
  // lazily through Tauri's asset protocol (dynamic import, like ipc.ts, so the mock
  // build never statically bundles Tauri). Falls back to the icon tile if absent.
  const [thumb, setThumb] = useState<string | undefined>(() =>
    isImage && source.type === "inline"
      ? `data:${mediaType};base64,${source.value}`
      : undefined,
  );
  useEffect(() => {
    if (!isImage || source.type !== "path") return;
    let alive = true;
    void import("@tauri-apps/api/core")
      .then(({ convertFileSrc }) => {
        if (alive) setThumb(convertFileSrc(source.value));
      })
      .catch(() => {
        /* no asset protocol (e.g. mock/browser) — keep the icon fallback */
      });
    return () => {
      alive = false;
    };
  }, [isImage, source.type, source.value]);

  return (
    <div
      className="group relative flex items-center gap-2 rounded-md border bg-muted/40 py-1 pl-1 pr-2"
      title={
        name ? `${name} · ${typeLabel} · ${formatBytes(bytes)}` : undefined
      }
    >
      <div className="flex size-9 shrink-0 items-center justify-center overflow-hidden rounded bg-background text-muted-foreground">
        {thumb ? (
          <img
            src={thumb}
            alt={label}
            className="size-9 rounded object-cover"
          />
        ) : (
          <FileText className="size-4" />
        )}
      </div>
      <div className="flex min-w-0 flex-col">
        <span className="max-w-32 truncate text-xs text-foreground">
          {label}
        </span>
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
          {typeLabel} · {formatBytes(bytes)}
        </span>
      </div>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="ml-1 shrink-0 text-muted-foreground hover:text-foreground"
        onClick={onRemove}
        title={name ? `Remove ${name}` : "Remove attachment"}
        aria-label="Remove attachment"
      >
        <X className="size-3" />
      </Button>
    </div>
  );
}

// Agent-mode pill (#266, RFC 0011). Per-session (and so per split pane) + persisted,
// colour-coded. Click cycles Plan → Act → Auto; ⌘. cycles the focused pane too
// (app-shell). A session with no explicit mode shows the `defaultMode` preference.
export function ModePill({ sessionId }: { sessionId: string }) {
  const defaultMode = usePrefsStore((s) => s.defaultMode);
  const explicit = useSessionModeStore((s) => s.modeBySession[sessionId]);
  const setMode = useSessionModeStore((s) => s.setMode);
  const mode = explicit ?? defaultMode;
  const meta = MODE_META[mode];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          title={`Mode: ${meta.label} — ${meta.description}`}
          aria-label={`Agent mode: ${meta.label}`}
          className={cn(
            "inline-flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1.5 text-xs font-medium transition-colors",
            meta.pillClass,
          )}
        >
          <span
            className={cn("size-1.5 shrink-0 rounded-full", meta.dotClass)}
          />
          {meta.label}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        {MODE_ORDER.map((m) => {
          const mMeta = MODE_META[m];
          return (
            <DropdownMenuItem
              key={m}
              onSelect={() => setMode(sessionId, m)}
              className="items-start gap-2"
            >
              <span
                className={cn(
                  "mt-1 size-1.5 shrink-0 rounded-full",
                  mMeta.dotClass,
                )}
              />
              <span className="flex min-w-0 flex-col">
                <span className="font-medium text-foreground">
                  {mMeta.label}
                </span>
                <span className="text-[11px] text-muted-foreground">
                  {mMeta.description}
                </span>
              </span>
              {m === mode && (
                <Check className="mt-0.5 ml-auto size-3.5 shrink-0 text-foreground" />
              )}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const IN_TAURI =
  globalThis.window !== undefined && "__TAURI_INTERNALS__" in globalThis.window;

/** Last path segment, for a compact label (full path shown on hover). */
function basename(path: string): string {
  const parts = path.replace(/[/\\]+$/, "").split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

// Shared styling for the workspace + branch chips in the bar below the composer
// (#606). `min-w-0` lets the inner label truncate when the bar is narrow.
const WORKSPACE_CHIP_CLASS =
  "inline-flex min-w-0 items-center gap-1.5 rounded-md border border-border bg-muted/40 px-2 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground data-[state=open]:bg-muted/70 data-[state=open]:text-foreground";

// The working directory a session's tools run in (#200). A filterable combobox
// (#210): a compact chip opens a popover with a Filter box, the recent workspaces
// (the active one highlighted), and Browse docked as a footer that opens the
// native OS folder picker. Outside Tauri (mock / `pnpm dev` in a browser) Browse
// falls back to a prompt so the UI stays exercisable.
function WorkspaceSelector({ sessionId }: { sessionId: string }) {
  const workspace = useSessionWorkspaceStore((s) => s.bySession[sessionId]);
  const recents = useSessionWorkspaceStore((s) => s.recents);
  const load = useSessionWorkspaceStore((s) => s.load);
  const setWorkspace = useSessionWorkspaceStore((s) => s.set);
  const path = workspace?.path;
  const branch = workspace?.gitBranch ?? null;
  const loaded = workspace !== undefined;

  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");

  // Branch-switch picker (#618 item 2). Lazy-loaded on open, like ModelChip.
  const [branchOpen, setBranchOpen] = useState(false);
  const [branches, setBranches] = useState<string[] | null>(null);
  const [loadingBranches, setLoadingBranches] = useState(false);

  // Fully disable the branch-change flash under `prefers-reduced-motion` (#618).
  // Gating the class in the component (not just CSS) means *zero* animation — no
  // background fade either — while the aria-live announcement still fires.
  const [reducedMotion, setReducedMotion] = useState(
    () =>
      typeof window !== "undefined" &&
      !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches,
  );
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const onChange = () => setReducedMotion(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  useEffect(() => {
    void load(sessionId);
  }, [sessionId, load]);

  // Flash the chip when the cwd's git branch *transitions* (#618) — an external
  // terminal checkout (or, once #2 lands, a self-switch) both surface as a
  // `workspace:branch-changed` store patch. A silent text swap is easy to miss,
  // so we draw the eye with a one-shot pulse + an aria-live announcement.
  //
  // The refs distinguish a true transition from "first render / different
  // session" (the store already no-ops equal values, but the component must not
  // flash on mount or when switching panes). `branch === null` (detached HEAD /
  // non-repo) hides the branch chip, so that transition flashes the folder chip
  // — the element that stays meaningful — instead.
  const prevSessionRef = useRef(sessionId);
  const prevBranchRef = useRef<string | null | undefined>(undefined);
  const [flashTarget, setFlashTarget] = useState<"branch" | "folder" | null>(
    null,
  );
  const [flashNonce, setFlashNonce] = useState(0);
  const [announce, setAnnounce] = useState("");

  useEffect(() => {
    // Session/pane switch: reset the baseline and never flash — the new
    // session's branch is a fresh context, not a transition.
    if (prevSessionRef.current !== sessionId) {
      prevSessionRef.current = sessionId;
      prevBranchRef.current = loaded ? branch : undefined;
      return;
    }
    // Not loaded yet: hold off so the initial load (undefined → value) isn't
    // read as a transition.
    if (!loaded) return;

    const prev = prevBranchRef.current;
    prevBranchRef.current = branch;

    if (prev === undefined) return; // first observation for this session
    if (prev === branch) return; // unchanged — no flash

    setFlashTarget(branch === null ? "folder" : "branch");
    setFlashNonce((n) => n + 1);
    setAnnounce(
      branch === null
        ? "Working branch changed: now in detached HEAD"
        : `Working branch changed to ${branch}`,
    );
    const timer = setTimeout(() => setFlashTarget(null), 1500);
    return () => clearTimeout(timer);
  }, [sessionId, branch, loaded]);

  const choose = useCallback(
    async (next: string) => {
      setOpen(false);
      if (next === path) return;
      try {
        await setWorkspace(sessionId, next);
      } catch {
        // Backend rejected the path (not a directory). Leave the cache unchanged.
      }
    },
    [sessionId, path, setWorkspace],
  );

  const browse = useCallback(async () => {
    setOpen(false);
    let chosen: string | null | undefined;
    if (IN_TAURI) {
      const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
      const result = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: path,
      });
      chosen = typeof result === "string" ? result : null;
    } else {
      chosen = globalThis.prompt?.("Working directory", path ?? "");
    }
    if (chosen) await choose(chosen);
  }, [path, choose]);

  // Lazy-load the branch list when the picker opens (#618) — reloaded on each
  // open so a new/renamed branch or an external checkout shows up. Empty list
  // (non-repo / detached HEAD) renders a disabled "No branches" row.
  const openBranchPicker = useCallback(
    (next: boolean) => {
      setBranchOpen(next);
      if (!next) return;
      setLoadingBranches(true);
      void ipc
        .listBranches(sessionId)
        .then((list) => setBranches(list))
        .catch(() => setBranches([]))
        .finally(() => setLoadingBranches(false));
    },
    [sessionId],
  );

  const switchTo = useCallback(
    async (next: string) => {
      setBranchOpen(false);
      if (next === branch) return; // already on it — skip a needless checkout
      try {
        await ipc.checkoutBranch(sessionId, next);
        // No manual store patch: checkout emits `workspace:branch-changed`, which
        // events.ts routes to applyBranchChanged — that relabels the chip and fires
        // the existing flash + announcement (the self-switch path, #618 item 2).
      } catch {
        // Backend rejected the checkout (dirty tree, unknown branch). Leave as-is.
      }
    },
    [sessionId, branch],
  );

  const query = filter.trim().toLowerCase();
  const filtered = query
    ? recents.filter((p) => p.toLowerCase().includes(query))
    : recents;

  return (
    <PopoverPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) setFilter("");
      }}
    >
      {/* Two chips: workspace (folder) and git branch. The folder chip is the
          Radix Popover Trigger, so a single `triggerRef` owns focus return on
          close. The branch chip is its own DropdownMenu branch picker (#618
          item 2) that lists and checks out branches; it is hidden on a detached
          HEAD / non-repo (`branch === null`). */}
      <div className="flex min-w-0 items-center gap-1.5">
        <PopoverPrimitive.Trigger asChild>
          <button
            type="button"
            key={`folder-${flashTarget === "folder" ? flashNonce : 0}`}
            className={cn(
              WORKSPACE_CHIP_CLASS,
              !reducedMotion && flashTarget === "folder" && "ff-branch-flash",
            )}
            aria-label={`Workspace: ${path ? basename(path) : "Loading"}`}
          >
            <Folder className="size-3.5 shrink-0" />
            <span className="truncate" title={path ?? undefined}>
              {path ? basename(path) : "Loading…"}
            </span>
            <ChevronsUpDown className="size-3 shrink-0 opacity-60" />
          </button>
        </PopoverPrimitive.Trigger>
        {branch ? (
          <DropdownMenu open={branchOpen} onOpenChange={openBranchPicker}>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                key={`branch-${flashTarget === "branch" ? flashNonce : 0}`}
                className={cn(
                  WORKSPACE_CHIP_CLASS,
                  !reducedMotion &&
                    flashTarget === "branch" &&
                    "ff-branch-flash",
                )}
                title={`Branch: ${branch}`}
                aria-label={`Git branch: ${branch}`}
              >
                <GitBranch className="size-3.5 shrink-0" />
                <span className="truncate">{branch}</span>
                <ChevronsUpDown className="size-3 shrink-0 opacity-60" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent
              align="start"
              className="max-h-72 w-56 overflow-y-auto"
            >
              {loadingBranches && branches === null ? (
                <DropdownMenuItem disabled>Loading…</DropdownMenuItem>
              ) : !branches || branches.length === 0 ? (
                <DropdownMenuItem disabled>No branches</DropdownMenuItem>
              ) : (
                branches.map((b) => (
                  <DropdownMenuItem
                    key={b}
                    onSelect={() => void switchTo(b)}
                    className="gap-2"
                  >
                    <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate" title={b}>
                      {b}
                    </span>
                    {b === branch && (
                      <Check className="ml-auto size-3.5 shrink-0 text-foreground" />
                    )}
                  </DropdownMenuItem>
                ))
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
        {/* Visually-hidden live region so the branch shift isn't visual-only
            (#618) — paired with the flash, announced to screen readers. */}
        <span role="status" aria-live="polite" className="sr-only">
          {announce}
        </span>
      </div>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          side="top"
          align="start"
          sideOffset={6}
          className="z-50 w-72 overflow-hidden rounded-xl border bg-popover text-popover-foreground shadow-lg outline-none data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95"
        >
          {/* Filter */}
          <div className="flex items-center gap-2 border-b px-2.5 py-2">
            <Search className="size-3.5 shrink-0 text-muted-foreground" />
            <input
              autoFocus
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && filtered[0]) {
                  e.preventDefault();
                  void choose(filtered[0]);
                }
              }}
              placeholder="Filter…"
              className="w-full bg-transparent text-xs text-foreground outline-none placeholder:text-muted-foreground"
              aria-label="Filter workspaces"
            />
          </div>

          {/* Recent workspaces */}
          <div className="max-h-64 overflow-y-auto p-1">
            {filtered.length === 0 ? (
              <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                {recents.length === 0 ? "No workspaces yet" : "No matches"}
              </p>
            ) : (
              filtered.map((p) => {
                const active = p === path;
                return (
                  <button
                    key={p}
                    type="button"
                    onClick={() => void choose(p)}
                    aria-current={active}
                    className={cn(
                      "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors",
                      active
                        ? "bg-accent text-accent-foreground"
                        : "text-foreground hover:bg-accent/60",
                    )}
                  >
                    <Folder className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1" title={p}>
                      <span className="block truncate text-[13px] font-medium">
                        {basename(p)}
                      </span>
                      <span className="block truncate text-[11px] text-muted-foreground">
                        {p}
                      </span>
                    </span>
                  </button>
                );
              })
            )}
          </div>

          {/* Browse — docked footer */}
          <button
            type="button"
            onClick={() => void browse()}
            className="flex w-full items-center gap-2 border-t px-2.5 py-2 text-left text-xs text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
          >
            <Folder className="size-3.5 shrink-0" />
            Browse…
          </button>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
