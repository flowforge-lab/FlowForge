import { describe, it, expect, beforeEach } from "vitest";
import { readCache, writeCache, clearCache } from "./message-cache";
import type { Message } from "@/bindings";

function makeMessage(id: string, sessionId: string): Message {
  return {
    id,
    sessionId,
    role: "user",
    content: "hello " + id,
    createdAt: Date.now(),
  };
}

describe("message-cache", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("readCache returns empty map when nothing is cached", () => {
    expect(readCache()).toEqual({});
  });

  it("writeCache + readCache round-trips messages", () => {
    const msgs = [makeMessage("m1", "s1"), makeMessage("m2", "s1")];
    writeCache("s1", msgs);
    const cached = readCache();
    expect(cached["s1"]).toHaveLength(2);
    expect(cached["s1"]![0].content).toBe("hello m1");
  });

  it("writeCache caps at 50 messages (keeps tail)", () => {
    const msgs = Array.from({ length: 80 }, (_, i) =>
      makeMessage(`m${i}`, "s1"),
    );
    writeCache("s1", msgs);
    const cached = readCache();
    expect(cached["s1"]).toHaveLength(50);
    // Should keep the LAST 50 (most recent)
    expect(cached["s1"]![0].id).toBe("m30");
    expect(cached["s1"]![49].id).toBe("m79");
  });

  it("clearCache removes a session entry", () => {
    writeCache("s1", [makeMessage("m1", "s1")]);
    writeCache("s2", [makeMessage("m2", "s2")]);
    clearCache("s1");
    const cached = readCache();
    expect(cached["s1"]).toBeUndefined();
    expect(cached["s2"]).toHaveLength(1);
  });

  it("readCache ignores non-prefixed localStorage keys", () => {
    localStorage.setItem("unrelated-key", "some value");
    writeCache("s1", [makeMessage("m1", "s1")]);
    const cached = readCache();
    expect(Object.keys(cached)).toEqual(["s1"]);
  });

  it("readCache returns empty on corrupted JSON", () => {
    localStorage.setItem("ff-msg-cache:s1", "{invalid json!!!");
    expect(readCache()).toEqual({});
  });
});
