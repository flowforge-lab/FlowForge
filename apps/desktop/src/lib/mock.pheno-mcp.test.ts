import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { PhenotypeMcpUnavailableEvent } from "./phenotype-mcp";

describe("MockIpc phenotype:mcp-unavailable (#301)", () => {
  it("fires on switch to a phenotype whose skill needs an absent server", async () => {
    const ipc = new MockIpc();
    const events: PhenotypeMcpUnavailableEvent[] = [];
    await ipc.onPhenotypeMcpUnavailable((e) => events.push(e));

    // `codon` activates `codegraph`, which requires the `codegraph` MCP server —
    // absent from the mock's running set.
    await ipc.switchPhenotype("codon");

    expect(events).toEqual([{ phenotype: "codon", servers: ["codegraph"] }]);
  });

  it("stays silent when no active skill requires an unavailable server", async () => {
    const ipc = new MockIpc();
    const events: PhenotypeMcpUnavailableEvent[] = [];
    await ipc.onPhenotypeMcpUnavailable((e) => events.push(e));

    await ipc.switchPhenotype("rust"); // rust-debugging + write-tests: no MCP deps
    await ipc.switchPhenotype("default"); // empty working set

    expect(events).toEqual([]);
  });

  it("fires on the per-pane binding path too", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    const events: PhenotypeMcpUnavailableEvent[] = [];
    await ipc.onPhenotypeMcpUnavailable((e) => events.push(e));

    await ipc.setSessionPhenotype(session.id, "codon");

    expect(events).toEqual([{ phenotype: "codon", servers: ["codegraph"] }]);
  });

  it("does not fire when clearing a per-pane binding", async () => {
    const ipc = new MockIpc();
    const session = await ipc.createSession();
    const events: PhenotypeMcpUnavailableEvent[] = [];
    await ipc.onPhenotypeMcpUnavailable((e) => events.push(e));

    await ipc.setSessionPhenotype(session.id, null);

    expect(events).toEqual([]);
  });
});
