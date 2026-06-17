import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc profile marketplace (SET.7)", () => {
  it("lists the full catalog by install count for an empty query", async () => {
    const ipc = new MockIpc();
    const results = await ipc.searchProfileMarketplace("");
    expect(results.length).toBeGreaterThan(1);
    const installs = results.map((r) => r.installs);
    expect(installs).toEqual([...installs].sort((a, b) => b - a));
  });

  it("ranks matches and drops non-matches for a query", async () => {
    const ipc = new MockIpc();
    const hits = await ipc.searchProfileMarketplace("devops");
    expect(hits[0].id).toBe("devops");
    expect(hits.every((h) => h.id === "devops")).toBe(true);

    expect(await ipc.searchProfileMarketplace("nomatch-xyz")).toEqual([]);
  });
});
