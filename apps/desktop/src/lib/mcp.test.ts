import { describe, expect, it } from "vitest";
import { mcpInstanceKey, mcpStateMeta, parseMcpToolName } from "./mcp";
import type { McpServerState } from "@/bindings/McpServerState";

describe("parseMcpToolName", () => {
  it("splits a bridged tool id into server + bare tool", () => {
    expect(parseMcpToolName("mcp__github__create_issue")).toEqual({
      server: "github",
      tool: "create_issue",
    });
  });

  it("keeps a bare tool name that itself contains underscores", () => {
    expect(parseMcpToolName("mcp__fs__read_text_file")).toEqual({
      server: "fs",
      tool: "read_text_file",
    });
  });

  it("returns null for non-MCP tools", () => {
    expect(parseMcpToolName("web_search")).toBeNull();
    expect(parseMcpToolName("todo")).toBeNull();
  });

  it("returns null for malformed namespacing", () => {
    expect(parseMcpToolName("mcp__github")).toBeNull(); // no tool segment
    expect(parseMcpToolName("mcp____tool")).toBeNull(); // empty server
  });
});

describe("mcpInstanceKey", () => {
  it("returns the bare id for a global instance (no scopeKey)", () => {
    expect(mcpInstanceKey({ id: "codegraph" })).toBe("codegraph");
    expect(mcpInstanceKey({ id: "codegraph", scopeKey: undefined })).toBe(
      "codegraph",
    );
  });

  it("keeps two same-id instances distinct by scope", () => {
    const a = mcpInstanceKey({ id: "codegraph", scopeKey: "flowforge" });
    const b = mcpInstanceKey({ id: "codegraph", scopeKey: "other-repo" });
    expect(a).not.toBe(b);
    expect(a).not.toBe(mcpInstanceKey({ id: "codegraph" }));
  });
});

describe("mcpStateMeta", () => {
  const states: McpServerState[] = [
    "starting",
    "running",
    "restarting",
    "failed",
    "disabled",
  ];

  it("resolves a label + token classes for every state", () => {
    for (const state of states) {
      const meta = mcpStateMeta(state);
      expect(meta.label).toBeTruthy();
      expect(meta.badgeClassName).toBeTruthy();
      expect(meta.dotClassName).toBeTruthy();
    }
  });

  it("uses destructive tokens for the failed state and the accent for running", () => {
    expect(mcpStateMeta("failed").badgeClassName).toContain("destructive");
    expect(mcpStateMeta("running").dotClassName).toContain("bg-primary");
  });
});
