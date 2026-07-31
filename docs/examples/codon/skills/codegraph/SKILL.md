---
name: codegraph
description: Code-aware navigation via the codegraph MCP server — query the symbol graph instead of grepping. Load it with tool_search before touching code.
version: 0.2.0
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

codegraph keeps a local, auto-syncing index of the workspace and answers
structural questions in a handful of calls — usually with zero file reads.

**The server documents itself.** Its MCP `initialize` response carries the
authoritative guide: when to call it, how to shape a query, how to read the
output, and the anti-patterns to avoid. Read that. This file deliberately does
NOT restate it — an out-of-date paraphrase is worse than no paraphrase, and this
skill previously shipped a tool menu in which four of five names did not exist.

## What the server cannot tell you

**One tool, and you must load it first.** codegraph advertises exactly one tool,
`codegraph_explore`, bridged by FlowForge as `mcp__codegraph__codegraph_explore`
(the bare name will not resolve). Upstream unlists the narrower tools on purpose
— one strong tool steers agents better than a menu, and everything they returned
now arrives inline. A `codegraph_callers` / `_impact` / `_search` / `_context` /
`_trace` is a name you invented, not a tool you have.

It is not in the default tool set: run `tool_search "codegraph"` before you touch
code. Nothing errors if you skip this — you simply fall into a grep loop where
every individual call succeeds, which is why the omission is easy to miss.

**Focused queries beat exhaustive ones.** `explore` is bounded by an output byte
budget, not by `maxFiles`; raising `maxFiles` above its default is a no-op,
since it can only trim. Padding the query with every symbol you can think of
dilutes ranking and returns *less*. Measured on a 737-file workspace: 5 names →
51 symbols across 7 files, complete; 12 names → 19 symbols across 2 files,
truncated. Four to six high-signal names is the sweet spot. If the answer is not
there, send a second focused `explore` rather than one bloated call.

**Working tree only.** Reading a blob at another ref (`git show <ref>:<file>`, a
PR diff) is shell work, as is anything not indexed as source — lockfiles, CI
logs, sqlite files.

**`codegraph affected` does not work for Rust.** It identifies tests by `tests/`
directory placement, so Rust's in-`src` `#[cfg(test)] mod tests` is invisible to
it. Measured on this workspace: it missed every unit-test module that actually
covered a change while returning the same large constant set of frontend tests
for any Rust file touched. Pick the crates to re-test yourself.

## Trust boundary

Cross-file resolution for Rust measures 86.7% upstream, near the bottom of the
benchmarked languages, so a missing edge on a trait impl, macro-generated item,
or dynamic dispatch is expected. Treat the graph as a fast way to find the code
to read, not as proof that you found every caller. Confirm structure with
`cargo check` and the test suite.

After a codegraph version upgrade, re-index (`codegraph index -f`): an index
built by an older engine keeps its older, sparser edges, and `codegraph status`
flags this. On this workspace the full rebuild takes about a second and recovered
roughly 4,000 previously unresolved edges.

## Freshness

codegraph auto-syncs on file changes (debounced) and reconciles on reconnect. If
a response prepends a `⚠️` staleness banner for a file, read that file directly
for its live content. Query it yourself — don't delegate exploration to
file-reading sub-agents, or the index becomes pure overhead.
