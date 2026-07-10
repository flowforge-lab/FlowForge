// Pure data-model occurrence generator for the in-thread find bar (#875). The
// authoritative match set comes from the backend `searchInSession` (FTS5 over
// message text + tool-call args + tool-result bodies, v11). Within each matching
// message we still need a *count* and a *step cursor*, and the DOM is the wrong
// place to compute either: sub-blocks fold by default, so a DOM walk either
// undercounts (folded → not in the tree) or lands on the wrong element (folded
// wrapper → "scroll to top of first response").
//
// This module derives occurrences directly from the same fields the backend
// indexes, in the same order the DOM renders them after every matching sub-block
// is force-opened. The find bar uses the result as the single source of truth
// for `m` and the active cursor; the DOM is consulted only to manufacture
// paintable Ranges for the active occurrence.
//
// Reasoning text (#574 thinking block) is intentionally *not* a source: the
// backend FTS5 index doesn't cover it. Searching it on the FE would re-introduce
// the exact kind of divergence (#875) that motivated this module.

import type { Message } from "@/bindings";
import { isWordChar, tokenizeQuery } from "@/lib/find-tokens";
import type { ToolStep } from "@/store/chat";

/**
 * The source field within a message that an occurrence was extracted from.
 * Matches the parts of the backend FTS5 index the FE can faithfully mirror
 * without the backend changing shape (`#679` + v11 tool-result body migration).
 */
export type OccurrenceSource = "content" | "tool-args" | "tool-result";

/**
 * One matchable span of text inside a message, paired with the stable id of the
 * collapser that, when folded, hides it from the DOM. `expandId` is undefined
 * for spans that are always visible (`content` of any rendered message — the
 * `[data-message-id]` wrapper is itself always rendered).
 */
export type SearchableSpan = {
  /** Stable identifier within the message: `tool-call:<id>` or `tool-result:<id>`
   *  for tool spans; undefined for the message content (single canonical span). */
  sourceId: string;
  text: string;
  /** Collapser id to force-open before the DOM walker can find this span. */
  expandId?: string;
  source: OccurrenceSource;
};

export type Occurrence = {
  messageId: string;
  source: OccurrenceSource;
  sourceId: string;
  /** Lowercased offset into the *lowercased* span text — matches the highlighter. */
  offset: number;
  /** Length of the matched token — same convention as `tokenizeQuery`. */
  length: number;
  /** Collapser id to force-open so this occurrence is reachable in the DOM. */
  expandId?: string;
};

/**
 * Extract every searchable span for a single message, in DOM order:
 *
 *   1. message content (always visible — `expandId` undefined),
 *   2. each live `ToolStep`'s arguments and result (force-open the step),
 *   3. each persisted `toolCalls[*].arguments` (force-open the step),
 *   4. each persisted `tool` message's body (force-open its `OutputBlock`).
 *
 * Source order matches the order each sub-block appears under the message's
 * wrapper: content first, then the assistant's tool-call/result subtree, then
 * the trailing `tool` rows. After force-open, this list is also the DOM order.
 */
export function extractSpans(
  message: Message,
  liveSteps?: ToolStep[],
): SearchableSpan[] {
  const spans: SearchableSpan[] = [];

  // 1. Message body — always rendered (the `data-message-id` wrapper itself).
  if (message.content) {
    spans.push({
      source: "content",
      sourceId: message.id,
      text: message.content,
    });
  }

  // 2. Live tool steps while streaming (or reactivated). Both args and result
  // are searched: the args because the backend indexes them on the persisted
  // assistant message's `toolCalls[*].arguments`; the result because v11 added
  // tool-result bodies. Live steps mount inside a `ToolStepBlock` that's folded
  // by default, so each needs an `expandId` so the find bar can force-open.
  if (liveSteps) {
    for (const step of liveSteps) {
      const argsText = step.args != null ? stringifyArgs(step.args) : "";
      if (argsText) {
        spans.push({
          source: "tool-args",
          sourceId: `tool-call:${step.callId}`,
          expandId: `tool-step:${step.callId}`,
          text: argsText,
        });
      }
      if (step.result) {
        spans.push({
          source: "tool-result",
          sourceId: `tool-result:${step.callId}`,
          expandId: `tool-step:${step.callId}`,
          text: step.result,
        });
      }
    }
  }

  // 3. Persisted tool-call args on the assistant message. Live steps already
  // covered these for the same callId; only emit a span for persisted calls
  // that don't have a live step (the post-stream / reloaded path — `args` are
  // captured here even if `result` already lives on the trailing `tool` row).
  for (const call of message.toolCalls ?? []) {
    if (liveSteps?.some((s) => s.callId === call.id)) continue;
    if (!call.arguments) continue;
    spans.push({
      source: "tool-args",
      sourceId: `tool-call:${call.id}`,
      expandId: `tool-step:${call.id}`,
      text: call.arguments,
    });
  }

  return spans;
}

/**
 * Extract standalone `tool` message bodies (persisted tool-result rows that
 * render in their own `[data-message-id]` wrapper, not nested under their
 * assistant). These are reachable in the search index via the messageId passed
 * to `buildSessionOccurrences`, but only have a meaningful `expandId` when
 * their body is folded (it is — see `OUTPUT_FOLD_THRESHOLD`).
 */
export function extractStandaloneToolSpan(message: Message): SearchableSpan[] {
  if (
    (message.role !== "tool" && message.role !== "system") ||
    !message.content
  ) {
    return [];
  }
  return [
    {
      source: "tool-result",
      sourceId: `tool-result-row:${message.id}`,
      expandId: `output:${message.id}`,
      text: message.content,
    },
  ];
}

/**
 * Walk a list of spans and emit one `Occurrence` per whole-token hit, in
 * source order. Mirrors `collectOccurrences` token semantics (#748): AND of
 * query tokens, whole-token boundary only, case-insensitive, de-duplicated
 * query tokens.
 */
export function collectOccurrencesFromSpans(
  messageId: string,
  spans: SearchableSpan[],
  query: string,
): Occurrence[] {
  const tokens = tokenizeQuery(query);
  if (tokens.length === 0) return [];

  const occurrences: Occurrence[] = [];
  for (const span of spans) {
    const hay = span.text.toLowerCase();
    if (hay.length === 0) continue;
    const hits: { at: number; len: number }[] = [];
    for (const token of tokens) {
      let at = hay.indexOf(token, 0);
      while (at !== -1) {
        const end = at + token.length;
        // Whole token only: neither neighbour may be a token character.
        // Mirror the DOM walker's predicate in `find-highlight.ts:74` so the
        // data-model list and the DOM range list never disagree on what
        // counts as a word boundary (#875). `hay` is already lowercased so
        // case doesn't change the boundary set.
        if (!isWordChar(hay[at - 1]) && !isWordChar(hay[end])) {
          hits.push({ at, len: token.length });
        }
        at = hay.indexOf(token, at + token.length);
      }
    }
    hits.sort((a, b) => a.at - b.at);
    for (const { at, len } of hits) {
      occurrences.push({
        messageId,
        source: span.source,
        sourceId: span.sourceId,
        offset: at,
        length: len,
        ...(span.expandId ? { expandId: span.expandId } : {}),
      });
    }
  }
  return occurrences;
}

/**
 * Build the ordered occurrence list for one session, restricted to messages
 * the backend `searchInSession` matched.
 *
 * Document order: message order in the caller-supplied array (which is the
 * transcript's authored order), then within each message the order returned by
 * `extractSpans`, then within each span by token offset. This matches the DOM
 * order produced by `collectOccurrences` *after* the find bar has forced every
 * distinct `expandId` open.
 *
 * Standalone `tool` rows carry their own `messageId` (their own
 * `[data-message-id]`), so they're indexed under their own id and don't nest
 * under the assistant — matches the on-screen render in `chat-view.tsx`.
 */
export function buildSessionOccurrences(
  messages: Message[],
  liveStepsByMessage: Record<string, ToolStep[] | undefined>,
  matchingMessageIds: Set<string>,
  query: string,
): Occurrence[] {
  if (matchingMessageIds.size === 0 || tokenizeQuery(query).length === 0) {
    return [];
  }
  const out: Occurrence[] = [];
  for (const message of messages) {
    if (!matchingMessageIds.has(message.id)) continue;
    const liveSteps = liveStepsByMessage[message.id];
    const spans =
      message.role === "tool" || message.role === "system"
        ? extractStandaloneToolSpan(message)
        : extractSpans(message, liveSteps);
    out.push(...collectOccurrencesFromSpans(message.id, spans, query));
  }
  return out;
}

/**
 * Distinct `expandId`s across a list of occurrences (in declaration order).
 * Consumed by the find bar to drive `forceOpenMany` before the DOM is walked.
 */
export function uniqueExpandIds(occurrences: Iterable<Occurrence>): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const occ of occurrences) {
    if (occ.expandId && !seen.has(occ.expandId)) {
      seen.add(occ.expandId);
      out.push(occ.expandId);
    }
  }
  return out;
}

function stringifyArgs(args: unknown): string {
  if (args == null) return "";
  if (typeof args === "string") return args;
  try {
    return JSON.stringify(args);
  } catch {
    return String(args);
  }
}
