import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

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
