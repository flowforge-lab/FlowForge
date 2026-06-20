---
name: codegraph
description: Code-aware navigation via the codegraph MCP server — query the symbol graph (definitions, callers, callees, impact) instead of grepping.
version: 0.1.0
author: tonytan4ever
mcp:
  - codegraph
keywords:
  - code
  - navigation
  - graph
  - callers
  - references
  - refactor
---
# Codegraph — navigate code as a graph

This skill declares the `codegraph` MCP server as its DNA: a phenotype that lists
this skill expects codegraph's tools to be available. codegraph keeps a local,
auto-syncing index of the workspace and answers structural questions in a handful
of calls — usually with zero file reads.

When the server is absent or not running, FlowForge warns on activation and these
tools are simply unavailable; fall back to `grep`/`glob`/`view`. codegraph only
helps when you query it directly — don't delegate exploration to file-reading
sub-agents, or the index becomes overhead.

## Tools (prefixed `mcp__codegraph__` once bridged)

- **`codegraph_explore`** — the primary tool. Returns the relevant symbols' source
  grouped by file plus a relationship map, in one call. Use it for almost
  anything: "how does X work", "how does X reach Y", or surveying an area. Answer
  from its output and stop.
- **`codegraph_search`** — locate a symbol by name across the codebase.
- **`codegraph_callers`** — every call site of a function (including callback
  registrations). Use before changing a signature.
- **`codegraph_callees`** — what a function calls.
- **`codegraph_impact`** — the impact radius of changing a symbol; run before an edit.
- **`codegraph_node`** — one symbol's details and full source + callers, or read a
  file like the `view` tool (with line numbers).
- **`codegraph_files`** — the indexed file structure (faster than scanning the FS).
- **`codegraph_status`** — index health and pending-sync status.

## When to use which

1. Starting a task or answering "how does X work?" -> `codegraph_explore`.
2. Just need where a symbol lives -> `codegraph_search`.
3. About to change a function -> `codegraph_callers` + `codegraph_impact` first.
4. Reading a specific symbol or file -> `codegraph_node`.
5. Only when the graph can't answer -> grep/glob/read.

## Freshness

codegraph auto-syncs on file changes (debounced) and reconciles on reconnect. If a
response prepends a `⚠️` staleness banner for a file, read that file directly for
its live content. Check `codegraph_status` if results look stale.
