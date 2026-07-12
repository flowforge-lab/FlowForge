// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

// Hoisted so the (hoisted) vi.mock factory can close over the spy.
const { searchMessages } = vi.hoisted(() => ({
  searchMessages: vi.fn(),
}));

vi.mock("@/lib/ipc", () => ({ ipc: { searchMessages } }));

import { useContentSearch } from "@/hooks/use-content-search";
import type { SearchHit } from "@/bindings/SearchHit";

function hit(messageId: string): SearchHit {
  return {
    sessionId: "s1",
    sessionTitle: null,
    messageId,
    role: "assistant",
    snippet: `<mark>x</mark> in ${messageId}`,
    createdAt: 0,
  };
}

async function tick(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  searchMessages.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useContentSearch", () => {
  it("returns no hits and isn't pending for an empty query", () => {
    const { result } = renderHook(() => useContentSearch(""));
    expect(result.current.hits).toEqual([]);
    expect(result.current.pending).toBe(false);
    expect(searchMessages).not.toHaveBeenCalled();
  });

  it("debounces before calling ipc.searchMessages, and is pending in between", async () => {
    searchMessages.mockResolvedValue([hit("a1")]);
    const { result, rerender } = renderHook(
      ({ q }) => useContentSearch(q, { debounceMs: 200 }),
      { initialProps: { q: "" } },
    );

    rerender({ q: "parser" });
    expect(result.current.pending).toBe(true);
    expect(searchMessages).not.toHaveBeenCalled();

    await tick(199);
    expect(searchMessages).not.toHaveBeenCalled();

    await tick(1);
    expect(searchMessages).toHaveBeenCalledWith("parser", 30);
    expect(result.current.pending).toBe(false);
    expect(result.current.hits.map((h) => h.messageId)).toEqual(["a1"]);
  });

  it("ignores a stale response that resolves after a newer query supersedes it", async () => {
    let resolveFirst: (hits: SearchHit[]) => void = () => {};
    const first = new Promise<SearchHit[]>((resolve) => {
      resolveFirst = resolve;
    });
    searchMessages.mockReturnValueOnce(first);
    searchMessages.mockResolvedValueOnce([hit("second")]);

    const { result, rerender } = renderHook(
      ({ q }) => useContentSearch(q, { debounceMs: 200 }),
      { initialProps: { q: "first" } },
    );

    await tick(200);
    expect(searchMessages).toHaveBeenCalledTimes(1);

    // A newer query supersedes the in-flight first request before it resolves.
    rerender({ q: "second" });
    await tick(200);
    expect(searchMessages).toHaveBeenCalledTimes(2);
    expect(result.current.pending).toBe(false);
    expect(result.current.hits.map((h) => h.messageId)).toEqual(["second"]);

    // The stale first request resolving afterwards must not clobber the result.
    await act(async () => {
      resolveFirst([hit("stale")]);
      await Promise.resolve();
    });
    expect(result.current.hits.map((h) => h.messageId)).toEqual(["second"]);
  });

  it("clears hits with no delay when the query is emptied", async () => {
    searchMessages.mockResolvedValue([hit("a1")]);
    const { result, rerender } = renderHook(
      ({ q }) => useContentSearch(q, { debounceMs: 200 }),
      { initialProps: { q: "parser" } },
    );

    await tick(200);
    expect(result.current.hits).toHaveLength(1);

    rerender({ q: "" });
    await tick(0);
    expect(result.current.hits).toEqual([]);
    expect(result.current.pending).toBe(false);
  });

  it("clears hits and stops pending when the search rejects", async () => {
    searchMessages.mockRejectedValue(new Error("boom"));
    const { result, rerender } = renderHook(
      ({ q }) => useContentSearch(q, { debounceMs: 200 }),
      { initialProps: { q: "" } },
    );

    rerender({ q: "parser" });
    await tick(200);

    expect(result.current.pending).toBe(false);
    expect(result.current.hits).toEqual([]);
  });
});
