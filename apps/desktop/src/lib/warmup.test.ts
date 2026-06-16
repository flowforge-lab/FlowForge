import { describe, expect, it } from "vitest";
import { WarmupTrigger } from "./warmup";

describe("WarmupTrigger", () => {
  it("fires immediately on the first call", () => {
    const now = 1000;
    let fired = 0;
    const t = new WarmupTrigger(
      () => (fired += 1),
      5000,
      () => now,
    );

    t.fire();
    expect(fired).toBe(1);
  });

  it("suppresses calls inside the throttle window", () => {
    let now = 1000;
    let fired = 0;
    const t = new WarmupTrigger(
      () => (fired += 1),
      5000,
      () => now,
    );

    t.fire();
    now = 3000; // +2s, still inside 5s window
    t.fire();
    now = 5999; // +4.999s
    t.fire();
    expect(fired).toBe(1);
  });

  it("fires again once the throttle window elapses", () => {
    let now = 1000;
    let fired = 0;
    const t = new WarmupTrigger(
      () => (fired += 1),
      5000,
      () => now,
    );

    t.fire();
    now = 6000; // exactly +5s — boundary is no longer inside the window
    t.fire();
    expect(fired).toBe(2);
  });

  it("re-arms relative to the last accepted fire, not the last attempt", () => {
    let now = 0;
    let fired = 0;
    const t = new WarmupTrigger(
      () => (fired += 1),
      5000,
      () => now,
    );

    t.fire(); // t=0 -> fires (1)
    now = 4000;
    t.fire(); // suppressed
    now = 8000; // +8s from last accepted fire
    t.fire(); // fires (2)
    now = 10000;
    t.fire(); // suppressed (only +2s from t=8000)
    expect(fired).toBe(2);
  });
});
