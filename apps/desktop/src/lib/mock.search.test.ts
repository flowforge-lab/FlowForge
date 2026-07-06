import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { Message } from "../bindings";

/** Reach into the mock's private message map to seed a fixture message. */
function pushMessage(ipc: MockIpc, sessionId: string, m: Message) {
  (ipc as unknown as { messages: Map<string, Message[]> }).messages
    .get(sessionId)
    ?.push(m);
}

function userMessage(sessionId: string, id: string, content: string): Message {
  return { id, sessionId, role: "user", content, createdAt: Date.now() };
}

describe("MockIpc searchInSession token matching (#748)", () => {
  async function seed() {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    pushMessage(ipc, session.id, userMessage(session.id, "m1", "run the turn"));
    pushMessage(
      ipc,
      session.id,
      userMessage(session.id, "m2", "just run here"),
    );
    pushMessage(ipc, session.id, userMessage(session.id, "m3", "overrun loop"));
    return { ipc, sessionId: session.id };
  }

  it("ANDs whole tokens, any order (run turn → only the message with both)", async () => {
    const { ipc, sessionId } = await seed();
    const hits = await ipc.searchInSession(sessionId, "turn run");
    expect(hits.map((h) => h.messageId)).toEqual(["m1"]);
  });

  it("matches whole tokens only — `run` skips `overrun`", async () => {
    const { ipc, sessionId } = await seed();
    const hits = await ipc.searchInSession(sessionId, "run");
    expect(hits.map((h) => h.messageId)).toEqual(["m1", "m2"]);
  });

  it("returns [] for a blank or punctuation-only query", async () => {
    const { ipc, sessionId } = await seed();
    expect(await ipc.searchInSession(sessionId, "   ")).toEqual([]);
    expect(await ipc.searchInSession(sessionId, "-.")).toEqual([]);
  });
});

describe("MockIpc web-search settings", () => {
  it("defaults to SearXNG with no endpoint and no key", async () => {
    const ipc = new MockIpc();
    const cfg = await ipc.getSearchConfig();
    expect(cfg.backend).toBe("searxNg");
    expect(cfg.baseUrl).toBeUndefined();
    expect(cfg.hasKey).toBe(false);
  });

  it("persists a configured SearXNG endpoint", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setSearchConfig(
      "searxNg",
      "https://searx.example.org",
    );
    expect(stored.baseUrl).toBe("https://searx.example.org");
    expect((await ipc.getSearchConfig()).baseUrl).toBe(
      "https://searx.example.org",
    );
  });

  it("treats a blank endpoint as unset", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setSearchConfig("searxNg", "   ");
    expect(stored.baseUrl).toBeUndefined();
  });

  it("never reports a key (secrets are a later phase)", async () => {
    const ipc = new MockIpc();
    const stored = await ipc.setSearchConfig("brave", undefined);
    expect(stored.backend).toBe("brave");
    expect(stored.hasKey).toBe(false);
  });
});
