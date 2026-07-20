// Pure helpers that fold a transcript into the per-turn render model (#413/#415).
// After a session reload the live step model (`toolStepsByMessage`) is empty, so a
// multi-step turn would otherwise render as a flat, unfolded, untimed list of
// tool-output rows (#331). We reconstruct each turn's steps + timing so the EXISTING
// `StepGroup` / `ToolStepBlock` render reloaded turns identically — reuse, not fork.
//
// Turn shape (live AND persisted): the backend mints one assistant message per
// tool-calling iteration (ff-agent `run_turn`), each carrying that iteration's prose
// in `content` + its tool call(s), interleaved with `tool` results, up to the next
// user message — the final answer lands on the turn's last assistant message. So a
// turn = everything between two user messages. Its steps come from the live
// `toolStepsByMessage` (keyed per assistant message id) when streaming, or are
// reconstructed from the persisted `tool` results on reload — matched per assistant.
// Intermediate assistant prose is interleaved between the steps as folded rows (#415).
//
// Kept free of React so the grouping + adapter are unit-testable in isolation (see
// turn-groups.test.ts), mirroring lib/steps.ts.

import type { Message } from "@/bindings";
import type { ToolCall } from "@/bindings/ToolCall";
import type { ToolStep } from "@/store/chat";
import { groupDurationMs } from "@/lib/steps";

/** Mode-switch markers are injected as Role::User for Bedrock positional reasons
 *  (#848) but are model-facing signals, not user-authored text. Hide from UI. */
function isModeSwitchMarker(m: Message): boolean {
  return m.role === "user" && m.content.startsWith("[system:");
}

/** An ordered row inside an assistant turn: a tool step, a chunk of the model's
 *  intermediate prose ("Now let me check …") that preceded the next tool call (#415),
 *  that iteration's reasoning, sitting where the thinking happened — immediately
 *  before the tool calls it produced (#574) — or a short/operational narration line
 *  that stays folded inside the step group as a compact "thought" (#687). */
export type TurnItem =
  | { kind: "step"; step: ToolStep }
  | { kind: "prose"; text: string; key: string }
  | { kind: "reasoning"; text: string; key: string }
  | { kind: "thought"; text: string; key: string };

/** One rendered transcript row. */
export type RenderGroup =
  | { kind: "user"; message: Message }
  | {
      kind: "assistant";
      /** The turn's final assistant message — drives the answer text, key, actions. */
      message: Message;
      /** Ordered prose + step rows, interleaved in message order (#415). */
      items: TurnItem[];
      /** Flat list of just the turn's steps — for the "N steps" count, duration, peek. */
      steps: ToolStep[];
      /** Chain-of-thought persisted on the turn (#375); "" when absent. */
      reasoning: string;
      /** Wall-clock span from step timing; null when timestamps are absent. */
      durationMs: number | null;
    }
  // A `tool`/`system` message with no assistant before it (e.g. the seeded
  // standalone rows in #331). Rendered as a bare output row, unchanged.
  | { kind: "loose"; message: Message };

/** Treat a missing or zero/sentinel `createdAt` as "no timing" so reconstructed
 *  duration is hidden (matching the `groupDurationMs` null contract) rather than
 *  rendering a meaningless "<1s". Real backend timestamps are non-zero epoch ms. */
function validTs(ts: number | undefined): number | undefined {
  return typeof ts === "number" && ts > 0 ? ts : undefined;
}

/** Parse a persisted `ToolCall.arguments` (a JSON string per the tool-calling
 *  protocol). Falls back to the raw string if it isn't valid JSON. */
function parseArgs(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

/**
 * Map a persisted `tool`/`system` message to the live {@link ToolStep} shape so it
 * feeds the existing StepGroup/ToolStepBlock unchanged.
 * - `tool` + `args` come from the matched `ToolCall`; absent when unmatched.
 * - `status` is always `"done"`: persisted messages carry no status/error signal, so
 *   red stays reserved for the live path (see chat-view.tsx).
 * - `startedAt`/`finishedAt` come from the `createdAt` gap (calling message → this
 *   result), yielding a per-step duration and a correct turn span; omitted when absent.
 */
export function persistedStepToToolStep(
  msg: Message,
  call: ToolCall | undefined,
  index: number,
  fallbackKeyId: string,
  prevCreatedAt: number | undefined,
): ToolStep {
  const step: ToolStep = {
    callId: msg.toolCallId ?? `${fallbackKeyId}:step-${index}`,
    tool: call?.name ?? msg.role,
    args: call ? parseArgs(call.arguments) : null,
    status: "done",
    result: msg.content,
  };
  const startedAt = validTs(prevCreatedAt);
  const finishedAt = validTs(msg.createdAt);
  if (startedAt != null) step.startedAt = startedAt;
  if (finishedAt != null) step.finishedAt = finishedAt;
  return step;
}

/** Reconstruct one assistant message's steps from the persisted `tool`/`system`
 *  results that followed it, matched to its `toolCalls` by `toolCallId`. */
function reconstructSteps(
  assistant: Message,
  followers: Message[],
): ToolStep[] {
  const calls = new Map<string, ToolCall>();
  for (const c of assistant.toolCalls ?? []) calls.set(c.id, c);
  let prevCreatedAt: number | undefined = assistant.createdAt;
  return followers.map((f, idx) => {
    const step = persistedStepToToolStep(
      f,
      f.toolCallId ? calls.get(f.toolCallId) : undefined,
      idx,
      assistant.id,
      prevCreatedAt,
    );
    prevCreatedAt = f.createdAt;
    return step;
  });
}

/**
 * Fold a transcript into ordered render groups. Each user message is its own group;
 * the run of non-user messages that follows is one assistant turn. Within a turn,
 * each assistant message contributes its prose then its reasoning (folded rows)
 * followed by its steps — live steps from `liveSteps[messageId]` when present, else
 * reconstructed from the following persisted `tool` results. A run with no assistant
 * message is emitted as bare `loose` rows (#331).
 *
 * @param liveSteps      The store's `toolStepsByMessage`; live steps win over
 *                       reconstruction (they carry real status/timing while streaming).
 * @param liveReasoning  The store's `reasoningByMessage`; the live stream wins over the
 *                       reasoning persisted on a message so streaming order matches the
 *                       persisted order on reload (#574).
 */
/**
 * Index of the last turn boundary — the last `user` message (including mode-switch
 * markers, which break a run even though they don't render; see the fold loop). The
 * transcript up to this index is immutable while the final turn streams, so a caller
 * can fold that prefix once and re-fold only `messages.slice(here)` per token commit.
 * Returns `-1` when there is no user message (fold the whole transcript as the tail).
 *
 * Because a turn never spans a user boundary, splitting the fold here is exact:
 * `[...foldTurns(msgs.slice(0, i)), ...foldTurns(msgs.slice(i))]` equals
 * `foldTurns(msgs)` for any `i` returned here (see turn-groups.test.ts). Scans from
 * the end, so the cost is O(active turn length), not O(transcript).
 */
export function lastTurnStart(messages: Message[]): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "user") return i;
  }
  return -1;
}

export function foldTurns(
  messages: Message[],
  liveSteps?: Record<string, ToolStep[]>,
  liveReasoning?: Record<string, string>,
): RenderGroup[] {
  const groups: RenderGroup[] = [];
  let i = 0;
  while (i < messages.length) {
    const m = messages[i];
    if (m.role === "user") {
      // Mode-switch markers are Role::User for Bedrock positional reasons (#848)
      // but are model-facing, not user-authored — skip them from the UI.
      if (!isModeSwitchMarker(m)) {
        groups.push({ kind: "user", message: m });
      }
      i++;
      continue;
    }

    // Gather the run of non-user messages up to the next user message — one turn.
    const run: Message[] = [];
    let j = i;
    for (; j < messages.length && messages[j].role !== "user"; j++) {
      run.push(messages[j]);
    }
    i = j;

    // Group each assistant message with the tool/system results that follow it.
    const blocks: { assistant: Message; followers: Message[] }[] = [];
    const leading: Message[] = [];
    for (const r of run) {
      if (r.role === "assistant") {
        blocks.push({ assistant: r, followers: [] });
      } else if (blocks.length) {
        blocks[blocks.length - 1].followers.push(r);
      } else {
        leading.push(r); // tool/system before any assistant — orphan (#331)
      }
    }

    // No assistant in this run → orphan tool/system rows, rendered bare (#331).
    if (!blocks.length) {
      for (const r of run) groups.push({ kind: "loose", message: r });
      continue;
    }
    // Any stray leading orphans render bare, before the turn.
    for (const r of leading) groups.push({ kind: "loose", message: r });

    const lastAssistant = blocks[blocks.length - 1].assistant;
    const items: TurnItem[] = [];
    const steps: ToolStep[] = [];
    let reasoning = "";
    for (const { assistant: a, followers } of blocks) {
      // Group-level reasoning is kept only for the no-steps fallback (a standalone
      // Thinking block; see chat-view.tsx). When the turn has steps, reasoning is
      // interleaved per iteration as a `reasoning` item below — never floated to the top.
      if (!reasoning && a.reasoning) reasoning = a.reasoning;
      // Push order per iteration is `prose → reasoning → steps` (#619): prose leads so
      // `segmentTurn` can split it off as a top-level block, leaving this iteration's
      // reasoning contiguous with the steps it produced (#574) inside one steps segment.
      //
      // Intermediate prose: a non-final assistant message's content is narration
      // between tool calls (#415). The final message's content is the answer,
      // rendered below the group — never a prose row.
      if (a !== lastAssistant && a.content.trim()) {
        items.push({ kind: "prose", text: a.content, key: a.id });
      }
      // This iteration's reasoning, in position — immediately before the steps it
      // produced (#574). The live stream wins over the persisted copy so streaming order
      // matches the persisted order on reload.
      const aReasoning = liveReasoning?.[a.id] ?? a.reasoning ?? "";
      if (aReasoning.trim()) {
        items.push({ kind: "reasoning", text: aReasoning, key: a.id });
      }
      const live = liveSteps?.[a.id];
      const aSteps =
        live && live.length > 0 ? live : reconstructSteps(a, followers);
      for (const step of aSteps) {
        items.push({ kind: "step", step });
        steps.push(step);
      }
    }

    groups.push({
      kind: "assistant",
      message: lastAssistant,
      items,
      steps,
      reasoning,
      durationMs: groupDurationMs(steps),
    });
  }
  return groups;
}

/** A chronological chunk of one assistant turn (#619). Intermediate prose is a
 *  standalone, top-level block; the contiguous reasoning + steps around it are one
 *  collapsible steps segment (a "N steps" group). Rendered in order, the turn reads
 *  `prose → steps → prose → steps → …` so intermediate narration is visible without
 *  expanding any group. */
export type TurnSegment =
  | { kind: "prose"; text: string; key: string }
  | { kind: "steps"; items: TurnItem[]; steps: ToolStep[]; key: string };

// Classification for intermediate prose (#687): decide whether a chunk of narration
// is substantive (hoisted to a full-width block between step groups) or a throwaway
// operational line (folded inside the step group as a compact "thought"). Constants
// are exported so the bands stay tuneable in one place.
//
// The key insight: when the model *formats* text (backticks, bold, lists, paragraph
// breaks) it means it to be read; plain operational narration ("Let me check…") does
// not. So we combine formatting presence + an operational-prefix check + length bands
// rather than a naive length threshold.
export const HAS_FORMATTING = /`[^`]+`|\*\*[^*]+\*\*|^\s*[-*]\s|\n\n/m;
export const OPERATIONAL_START =
  /^(let me|now |wait|i'll|i need|let's|hmm|ok[, ]|actually|first|next |then |so |checking|looking|running|trying|verifying)/i;
/** Below this, prose is always a thought. */
export const SHORT = 120;
/** At/above this, unformatted prose is still substantive. */
export const LONG = 350;

/** Whether an intermediate-prose chunk is substantive (→ top-level block) vs a
 *  short/operational "thought" (→ folded inside the step group). See #687. */
export function isSubstantiveProse(text: string): boolean {
  const t = text.trim();
  if (t.length < SHORT) return false; // very short = always thought
  if (OPERATIONAL_START.test(t)) return false; // "Let me check…" = thought even if longish
  if (HAS_FORMATTING.test(t)) return true; // has formatting = substantive
  return t.length >= LONG; // long unformatted = still substantive
}

/**
 * Split a turn's ordered {@link TurnItem}s into {@link TurnSegment}s: a *substantive*
 * `prose` item becomes its own standalone segment, while a short/operational one is
 * demoted to a folded `thought` that stays inside the surrounding `steps` segment
 * (#687). Each run of contiguous `reasoning`/`step`/`thought` items collapses into one
 * `steps` segment (carrying its own flat `steps` for the per-group "N steps" count).
 * Empty steps segments are dropped, so a turn with no prose yields a single steps
 * segment — identical to the pre-split render.
 *
 * Pure and React-free (unit-tested in turn-groups.test.ts); the reorder in `foldTurns`
 * to `prose → reasoning → steps` keeps each iteration's reasoning bound to its steps
 * inside one segment (#574).
 */
export function segmentTurn(items: TurnItem[]): TurnSegment[] {
  const segments: TurnSegment[] = [];
  let buffer: TurnItem[] = [];
  let bufferSteps: ToolStep[] = [];

  const flush = () => {
    if (buffer.length > 0) {
      // Key off the first tool call in the run so the key stays stable while
      // streaming: a step can be buffered before its iteration's reasoning tokens
      // arrive, and pinning to `buffer[0]` would flip `callId` -> `messageId`
      // mid-stream, remounting the group and dropping its collapse state (#629). A
      // reasoning-only run (thinking, no tool calls) has no step, so fall back to
      // that item's `key`.
      const firstStep = buffer.find(
        (it): it is Extract<TurnItem, { kind: "step" }> => it.kind === "step",
      );
      const first = buffer[0];
      segments.push({
        kind: "steps",
        items: buffer,
        steps: bufferSteps,
        key:
          firstStep?.step.callId ??
          (first.kind === "step" ? first.step.callId : first.key),
      });
    }
    buffer = [];
    bufferSteps = [];
  };

  for (const it of items) {
    if (it.kind === "prose") {
      if (isSubstantiveProse(it.text)) {
        flush();
        segments.push({ kind: "prose", text: it.text, key: it.key });
      } else {
        // Short/operational prose stays inside the step group as a folded thought
        // (#687) rather than a full-width block.
        buffer.push({ kind: "thought", text: it.text, key: it.key });
      }
    } else {
      buffer.push(it);
      if (it.kind === "step") bufferSteps.push(it.step);
    }
  }
  flush();
  return segments;
}
