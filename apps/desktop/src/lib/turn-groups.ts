// Pure helpers that fold a persisted `Message[]` into the same per-turn render
// model the live path uses (#413). After a session reload the live step model
// (`toolStepsByMessage`) is empty, so a multi-step turn would otherwise render as a
// flat, unfolded, untimed list of tool-output rows (#331). Here we reconstruct each
// turn's `ToolStep[]` + timing from the persisted messages so the EXISTING
// `StepGroup` / `ToolStepBlock` render reloaded turns identically — reuse, not fork.
//
// Turn boundaries are USER messages: the backend persists one assistant message per
// tool call (sometimes several parallel calls), interleaved with `tool` results, up
// to the next user message — the final answer lands on the turn's last assistant
// message. So a turn = everything between two user messages, and its steps are the
// `tool`/`system` results, each matched to its `ToolCall` (by `toolCallId`) across
// the turn's assistant messages.
//
// Kept free of React so the grouping + adapter are unit-testable in isolation (see
// turn-groups.test.ts), mirroring lib/steps.ts.

import type { Message } from "@/bindings";
import type { ToolCall } from "@/bindings/ToolCall";
import type { ToolStep } from "@/store/chat";
import { groupDurationMs } from "@/lib/steps";

/** One rendered transcript row. */
export type RenderGroup =
  | { kind: "user"; message: Message }
  | {
      kind: "assistant";
      /** The turn's final assistant message — drives the answer text, key, actions. */
      message: Message;
      /** Steps reconstructed from the turn's persisted tool/system messages ([] when none). */
      steps: ToolStep[];
      /** Chain-of-thought persisted on the turn (#375); "" when absent. */
      reasoning: string;
      /** Wall-clock span from persisted `createdAt`; null when timestamps are absent. */
      durationMs: number | null;
    }
  // A `tool`/`system` message with no assistant in its run (e.g. the seeded
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

/**
 * Fold a transcript into ordered render groups. Each user message is its own group;
 * the run of non-user messages that follows is one assistant turn (its tool/system
 * results become steps). A run with no assistant message is emitted as bare `loose`
 * rows (#331).
 */
export function foldTurns(messages: Message[]): RenderGroup[] {
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

    // Collect every tool call declared across the turn's assistant messages, the
    // turn's representative (last) assistant, and its opening reasoning.
    const calls = new Map<string, ToolCall>();
    let lastAssistant: Message | undefined;
    let reasoning = "";
    for (const r of run) {
      if (r.role !== "assistant") continue;
      lastAssistant = r;
      if (!reasoning && r.reasoning) reasoning = r.reasoning;
      for (const c of r.toolCalls ?? []) calls.set(c.id, c);
    }

    // No assistant in this run → orphan tool/system rows, rendered bare (#331).
    if (!lastAssistant) {
      for (const r of run) groups.push({ kind: "loose", message: r });
      i = j;
      continue;
    }

    // Reconstruct steps from the tool/system results, timing from the createdAt gaps.
    const steps: ToolStep[] = [];
    let prevCreatedAt: number | undefined;
    for (const r of run) {
      if (r.role === "tool" || r.role === "system") {
        const call = r.toolCallId ? calls.get(r.toolCallId) : undefined;
        steps.push(
          persistedStepToToolStep(
            r,
            call,
            steps.length,
            lastAssistant.id,
            prevCreatedAt,
          ),
        );
      }
      prevCreatedAt = r.createdAt;
    }

    groups.push({
      kind: "assistant",
      message: lastAssistant,
      steps,
      reasoning,
      durationMs: groupDurationMs(steps),
    });
    i = j;
  }
  return groups;
}
