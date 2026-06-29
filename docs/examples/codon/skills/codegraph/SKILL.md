---
name: codegraph
description: Code-aware navigation via the codegraph MCP server — query the symbol graph (definitions, neighborhoods, call paths) in a few calls instead of grepping.
version: 0.1.1
author: tonytan4ever
mcp:
  - codegraph
keywords:
  - code
  - navigation
  - graph
  - references
  - call-path
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

## Invoking the tools

The tools below are named as codegraph advertises them. FlowForge bridges every
MCP tool under a namespaced id, so you must call them with the `mcp__codegraph__`
prefix — for example, invoke `codegraph_context` as
`mcp__codegraph__codegraph_context`. Calling the bare name will not resolve.

## Tools

- **`codegraph_context`** — the PRIMARY tool; call it FIRST for any "how does X
  work", architecture, or bug question. Give it a `task` description; it returns
  entry points + related symbols + key code in one call, usually answering with
  no further search or file reads.
- **`codegraph_explore`** — source of SEVERAL related symbols grouped by file, in
  one capped call. Its `query` is a bag of symbol/file names (not a question).
  The returned source is verbatim and Read-equivalent — don't re-open shown files.
- **`codegraph_search`** — quick symbol search by name; returns locations only (no
  code). Use it when you just need where a symbol lives.
- **`codegraph_node`** — one symbol's location, signature, and callers/callees
  trail; pass `includeCode` to get the verbatim body.
- **`codegraph_trace`** — the call path between two symbols ("how does `from` reach
  `to`?"), with each hop's body inlined, in one call. Ideal for flow questions.

## When to use which

1. Starting a task or answering "how does X work?" -> `codegraph_context`.
2. Surveying several symbols/files you can already name -> `codegraph_explore`.
3. Just need where a symbol lives -> `codegraph_search`.
4. One symbol's details or body -> `codegraph_node`.
5. "How does A reach B?" (a flow or call path) -> `codegraph_trace`.
6. Only when the graph can't answer -> grep/glob/read.

## Freshness

codegraph auto-syncs on file changes (debounced) and reconciles on reconnect. If a
response prepends a `⚠️` staleness banner for a file, read that file directly for
its live content.
