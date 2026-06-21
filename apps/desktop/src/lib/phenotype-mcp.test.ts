import { describe, expect, it } from "vitest";

import { describeUnavailable, unavailableToastBody } from "./phenotype-mcp";

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
