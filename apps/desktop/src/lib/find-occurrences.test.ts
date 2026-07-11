// Tests for the data-model occurrence generator (#875). Pure: no React, no DOM,
// no chat-store. Coverage:
//   • content / tool-args / tool-result extraction in DOM order,
//   • whole-token AND semantics mirroring `tokenizeQuery` (#748),
//   • distinct `expandId` aggregation (one per callId even with many hits),
//   • standalone `tool` rows render under their own expandId,
//   • reasoning is *not* searchable (parity with the backend FTS5 index — see
//     `find-occurrences.ts` for rationale),
//   • empty query / no matches / non-matching message are all `[]`.

import { describe, expect, it } from "vitest";

import type { Message } from "@/bindings";
import type { ToolStep } from "@/store/chat";
import {
  buildSessionOccurrences,
  collectOccurrencesFromSpans,
  extractSpans,
  extractStandaloneToolSpan,
  uniqueExpandIds,
  type SearchableSpan,
} from "@/lib/find-occurrences";

const NOW = 1_700_000_000_000;

function userMsg(id: string, content: string): Message {
  return { id, sessionId: "s", role: "user", content, createdAt: NOW };
}

function assistantMsg(
  id: string,
  content: string,
  toolCalls: Array<{ id: string; name: string; arguments: string }> = [],
): Message {
  return {
    id,
    sessionId: "s",
    role: "assistant",
    content,
    ...(toolCalls.length ? { toolCalls } : {}),
    createdAt: NOW,
  };
}

function toolMsg(id: string, content: string): Message {
  return { id, sessionId: "s", role: "tool", content, createdAt: NOW };
}

function toolStep(callId: string, args: unknown, result?: string): ToolStep {
  const step: ToolStep = {
    callId,
    tool: "bash",
    args,
    status: "done",
    ...(result !== undefined ? { result } : {}),
  };
  return step;
}

describe("extractSpans (#875)", () => {
  it("returns one content span for a plain assistant message", () => {
    const spans = extractSpans(assistantMsg("a1", "run the turn"), []);
    expect(spans).toHaveLength(1);
    expect(spans[0]).toMatchObject({
      source: "content",
      sourceId: "a1",
      text: "run the turn",
    });
    expect(spans[0].expandId).toBeUndefined();
  });

  it("emits live tool-step args + result with a shared expandId per callId", () => {
    const step = toolStep("c1", { command: "ls -al" }, "out\nmore out");
    const spans = extractSpans(assistantMsg("a1", ""), [step]);
    expect(spans).toEqual([
      {
        source: "tool-args",
        sourceId: "tool-call:c1",
        expandId: "tool-step:c1",
        text: JSON.stringify({ command: "ls -al" }),
      },
      {
        source: "tool-result",
        sourceId: "tool-result:c1",
        expandId: "tool-step:c1",
        text: "out\nmore out",
      },
    ]);
  });

  it("falls back to persisted toolCall arguments when no live step matches", () => {
    const spans = extractSpans(
      assistantMsg("a1", "", [
        { id: "c1", name: "bash", arguments: '{"command":"ls"}' },
      ]),
      [],
    );
    expect(spans).toEqual([
      {
        source: "tool-args",
        sourceId: "tool-call:c1",
        expandId: "tool-step:c1",
        text: '{"command":"ls"}',
      },
    ]);
  });

  it("does not double-count args shared by persisted call + live step", () => {
    const live = toolStep("c1", { command: "ls" }, "ok");
    const spans = extractSpans(
      assistantMsg("a1", "all done", [
        { id: "c1", name: "bash", arguments: '{"command":"ls"}' },
      ]),
      [live],
    );
    // Only the live step's span survives; persisted args are skipped (live wins).
    expect(spans.map((s) => s.source)).toEqual([
      "content",
      "tool-args",
      "tool-result",
    ]);
  });
});

describe("extractStandaloneToolSpan (#875)", () => {
  it("yields one tool-result span keyed by the message id for persisted tool rows", () => {
    const span = extractStandaloneToolSpan(toolMsg("tr1", "long output"))[0];
    expect(span).toMatchObject({
      source: "tool-result",
      sourceId: "tool-result-row:tr1",
      expandId: "output:tr1",
      text: "long output",
    });
  });

  it("returns [] for a user or assistant message", () => {
    expect(extractStandaloneToolSpan(userMsg("u1", "hi"))).toEqual([]);
    expect(extractStandaloneToolSpan(assistantMsg("a1", "hi"))).toEqual([]);
  });
});

describe("collectOccurrencesFromSpans (#875)", () => {
  it("emits one Occurrence per whole-token hit, in source/offset order", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "run the turn then run",
    };
    const occs = collectOccurrencesFromSpans("a1", [span], "run");
    expect(occs.map((o) => [o.offset, o.length])).toEqual([
      [0, 3],
      [18, 3],
    ]);
  });

  it("ANDs whole tokens, any order (run turn → both must appear in a span)", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "run the turn",
    };
    // `turn run` matches: both tokens appear once each, in any order.
    expect(collectOccurrencesFromSpans("a1", [span], "turn run")).toHaveLength(
      2,
    );
    expect(collectOccurrencesFromSpans("a1", [span], "run turn")).toHaveLength(
      2,
    );
  });

  it("does not match a token inside a larger word", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "overrun running run",
    };
    const occs = collectOccurrencesFromSpans("a1", [span], "run");
    expect(occs).toHaveLength(1);
    expect(occs[0].offset).toBe(16);
  });

  it("keeps case folding but preserves hit length in original chars", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "Run run RUN",
    };
    const occs = collectOccurrencesFromSpans("a1", [span], "run");
    // Three whole-token hits.
    expect(occs.map((o) => o.length)).toEqual([3, 3, 3]);
  });

  it("returns [] for a blank or punctuation-only query", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "run turn",
    };
    expect(collectOccurrencesFromSpans("a1", [span], "   ")).toEqual([]);
    expect(collectOccurrencesFromSpans("a1", [span], "---")).toEqual([]);
  });

  it("returns [] when a span's text is empty", () => {
    const span: SearchableSpan = {
      source: "content",
      sourceId: "a1",
      text: "",
    };
    expect(collectOccurrencesFromSpans("a1", [span], "run")).toEqual([]);
  });
});

describe("buildSessionOccurrences (#875)", () => {
  it("only indexes messages in `matchingMessageIds`", () => {
    const messages = [userMsg("u1", "run"), assistantMsg("a1", "run")];
    const occs = buildSessionOccurrences(messages, {}, new Set(["a1"]), "run");
    expect(occs).toHaveLength(1);
    expect(occs[0].messageId).toBe("a1");
  });

  it("iterates messages in supplied array order (transcript order)", () => {
    const messages = [
      assistantMsg("a2", "second run"),
      assistantMsg("a1", "first run"),
    ];
    const occs = buildSessionOccurrences(
      messages,
      {},
      new Set(["a1", "a2"]),
      "run",
    );
    expect(occs.map((o) => o.messageId)).toEqual(["a2", "a1"]);
  });

  it("renders a standalone tool row under its own messageId + expandId", () => {
    const messages = [
      assistantMsg("a1", "all done"),
      toolMsg("tr1", "run here"),
    ];
    const occs = buildSessionOccurrences(messages, {}, new Set(["tr1"]), "run");
    expect(occs).toHaveLength(1);
    expect(occs[0]).toMatchObject({
      messageId: "tr1",
      source: "tool-result",
      expandId: "output:tr1",
    });
  });

  it("drops an empty query / no matches down to []", () => {
    const messages = [assistantMsg("a1", "hello world")];
    expect(
      buildSessionOccurrences(messages, {}, new Set(["a1"]), "   "),
    ).toEqual([]);
    expect(buildSessionOccurrences(messages, {}, new Set([]), "hello")).toEqual(
      [],
    );
  });
});

describe("uniqueExpandIds (#875)", () => {
  it("preserves first-seen order, deduplicating by id", () => {
    const a = {
      messageId: "m",
      source: "tool-args" as const,
      sourceId: "tool-call:c1",
      offset: 0,
      length: 1,
      expandId: "tool-step:c1",
    };
    const b = {
      messageId: "m",
      source: "tool-result" as const,
      sourceId: "tool-result:c1",
      offset: 0,
      length: 1,
      expandId: "tool-step:c1",
    };
    const c = {
      messageId: "m",
      source: "tool-args" as const,
      sourceId: "tool-call:c2",
      offset: 0,
      length: 1,
      expandId: "tool-step:c2",
    };
    expect(uniqueExpandIds([a, b, c])).toEqual([
      "tool-step:c1",
      "tool-step:c2",
    ]);
  });

  it("skips occurrences with no expandId (content — always visible)", () => {
    const a = {
      messageId: "m",
      source: "content" as const,
      sourceId: "m",
      offset: 0,
      length: 1,
    };
    const b = {
      messageId: "m",
      source: "tool-args" as const,
      sourceId: "tool-call:c1",
      offset: 0,
      length: 1,
      expandId: "tool-step:c1",
    };
    expect(uniqueExpandIds([a, a, b])).toEqual(["tool-step:c1"]);
  });
});
