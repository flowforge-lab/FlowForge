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

/** An ordered row inside an assistant turn: a tool step, or a chunk of the model's
 *  intermediate prose ("Now let me check …") that preceded the next tool call (#415). */
export type TurnItem =
  | { kind: "step"; step: ToolStep }
  | { kind: "prose"; text: string; key: string };

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
 * each assistant message contributes its prose (folded row) followed by its steps —
 * live steps from `liveSteps[messageId]` when present, else reconstructed from the
 * following persisted `tool` results. A run with no assistant message is emitted as
 * bare `loose` rows (#331).
 *
 * @param liveSteps  The store's `toolStepsByMessage`; live steps win over
 *                   reconstruction (they carry real status/timing while streaming).
 */
export function foldTurns(
  messages: Message[],
  liveSteps?: Record<string, ToolStep[]>,
): RenderGroup[] {
  const groups: RenderGroup[] = [];
  let i = 0;
  while (i < messages.length) {
    const m = messages[i];
    if (m.role === "user") {
      groups.push({ kind: "user", message: m });
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
      if (!reasoning && a.reasoning) reasoning = a.reasoning;
      // Intermediate prose: a non-final assistant message's content is narration
      // between tool calls (#415). The final message's content is the answer,
      // rendered below the group — never a prose row.
      if (a !== lastAssistant && a.content.trim()) {
        items.push({ kind: "prose", text: a.content, key: a.id });
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
