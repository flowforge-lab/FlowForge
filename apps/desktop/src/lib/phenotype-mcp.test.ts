import { describe, expect, it } from "vitest";

import {
  describeUnavailable,
  unavailableServerDetails,
  unavailableToastBody,
} from "./phenotype-mcp";
import type { McpServerStatus } from "@/bindings/McpServerStatus";

describe("describeUnavailable", () => {
  it("uses the singular form for one server", () => {
    expect(
      describeUnavailable({ phenotype: "codon", servers: ["codegraph"] }),
    ).toBe("codon needs the codegraph MCP server, which is not available.");
  });

  it("uses the plural form and lists every server for many", () => {
    expect(
      describeUnavailable({
        phenotype: "codon",
        servers: ["codegraph", "github"],
      }),
    ).toBe(
      "codon needs 2 MCP servers (codegraph, github) that are not available.",
    );
  });
});

describe("unavailableToastBody", () => {
  it("appends the fallback hint with the singular 'it' for one server", () => {
    expect(
      unavailableToastBody({ phenotype: "codon", servers: ["codegraph"] }),
    ).toBe(
      "codon needs the codegraph MCP server, which is not available. Its grep/glob fallbacks still work — add or start it in MCP settings.",
    );
  });

  it("uses the plural 'them' for multiple servers", () => {
    expect(
      unavailableToastBody({
        phenotype: "codon",
        servers: ["codegraph", "github"],
      }),
    ).toBe(
      "codon needs 2 MCP servers (codegraph, github) that are not available. Its grep/glob fallbacks still work — add or start them in MCP settings.",
    );
  });
});

function status(over: Partial<McpServerStatus>): McpServerStatus {
  return {
    id: "codegraph",
    state: "failed",
    toolCount: 0,
    restarts: 0,
    ...over,
  };
}

describe("unavailableServerDetails", () => {
  it("returns the actual spawn error when the server is present but failing", () => {
    const statusById = new Map([
      [
        "codegraph",
        status({
          lastError:
            "failed to spawn MCP server 'codegraph': No such file or directory",
        }),
      ],
    ]);
    expect(
      unavailableServerDetails(
        { phenotype: "codon", servers: ["codegraph"] },
        statusById,
      ),
    ).toEqual([
      {
        server: "codegraph",
        detail:
          "failed to spawn MCP server 'codegraph': No such file or directory",
      },
    ]);
  });

  it("notes a server absent from mcp.json", () => {
    expect(
      unavailableServerDetails(
        { phenotype: "codon", servers: ["codegraph"] },
        new Map(),
      ),
    ).toEqual([{ server: "codegraph", detail: "not configured in mcp.json" }]);
  });

  it("falls back to the transient state when present without an error", () => {
    const statusById = new Map([
      ["codegraph", status({ state: "restarting", lastError: undefined })],
    ]);
    expect(
      unavailableServerDetails(
        { phenotype: "codon", servers: ["codegraph"] },
        statusById,
      ),
    ).toEqual([
      { server: "codegraph", detail: "restarting, no tools available yet" },
    ]);
  });

  it("preserves order across multiple servers", () => {
    const result = unavailableServerDetails(
      { phenotype: "codon", servers: ["codegraph", "github"] },
      new Map([["codegraph", status({ lastError: "boom" })]]),
    );
    expect(result.map((r) => r.server)).toEqual(["codegraph", "github"]);
    expect(result[1].detail).toBe("not configured in mcp.json");
  });
});
