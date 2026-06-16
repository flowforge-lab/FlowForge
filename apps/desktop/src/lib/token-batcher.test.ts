import { describe, expect, it } from "vitest";
import { TokenBatcher } from "./token-batcher";
import type { TokenEvent } from "@/bindings";

const tok = (messageId: string, delta: string): TokenEvent => ({
  sessionId: "s1",
  messageId,
  delta,
});

describe("TokenBatcher", () => {
  it("coalesces tokens for one message into a single flush per tick", () => {
    let tick: (() => void) | null = null;
    const flushed: TokenEvent[] = [];
    const b = new TokenBatcher(
      (e) => flushed.push(e),
      (cb) => {
        tick = cb;
      },
    );

    b.push(tok("m1", "Hel"));
    b.push(tok("m1", "lo"));
    b.push(tok("m1", " world"));
    expect(flushed).toHaveLength(0); // nothing until the tick fires

    tick!();
    expect(flushed).toEqual([tok("m1", "Hello world")]);
  });

  it("schedules only once per tick, then re-arms on the next token", () => {
    let scheduleCount = 0;
    let tick: (() => void) | null = null;
    const flushed: TokenEvent[] = [];
    const b = new TokenBatcher(
      (e) => flushed.push(e),
      (cb) => {
        scheduleCount += 1;
        tick = cb;
      },
    );

    b.push(tok("m1", "a"));
    b.push(tok("m1", "b"));
    expect(scheduleCount).toBe(1);

    tick!();
    expect(flushed).toEqual([tok("m1", "ab")]);

    b.push(tok("m1", "c"));
    expect(scheduleCount).toBe(2);
    tick!();
    expect(flushed).toEqual([tok("m1", "ab"), tok("m1", "c")]);
  });

  it("keeps separate buffers per message, preserving first-seen order", () => {
    let tick: (() => void) | null = null;
    const flushed: TokenEvent[] = [];
    const b = new TokenBatcher(
      (e) => flushed.push(e),
      (cb) => {
        tick = cb;
      },
    );

    b.push(tok("m1", "A"));
    b.push(tok("m2", "X"));
    b.push(tok("m1", "B"));
    b.push(tok("m2", "Y"));

    tick!();
    expect(flushed).toEqual([tok("m1", "AB"), tok("m2", "XY")]);
  });

  it("drain flushes synchronously before a non-token event is handled", () => {
    const order: string[] = [];
    const b = new TokenBatcher(
      (e) => order.push(`token:${e.delta}`),
      () => {
        /* never auto-fires in this test */
      },
    );

    b.push(tok("m1", "final"));
    b.drain();
    order.push("done");

    expect(order).toEqual(["token:final", "done"]);
  });

  it("drain is a no-op when nothing is pending", () => {
    let flushes = 0;
    const b = new TokenBatcher(
      () => {
        flushes += 1;
      },
      () => {},
    );
    b.drain();
    expect(flushes).toBe(0);
  });
});
