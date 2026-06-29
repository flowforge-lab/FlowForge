---
name: pr-review
description: Scope a pull-request review to the changed hunks. Fetch the diff once, reason about the change, and do not spider the call graph.
version: 0.1.0
author: isaacm
keywords:
  - review
  - pull-request
  - diff
  - pr
  - scope
---
# PR review -- stay scoped to the diff

Activate this skill when your task is to review a pull request. It overrides the
codon persona's "codegraph first" guidance for the duration of the review: a
review verifies the change, not the codebase.

## Fetch once, reuse

Get the PR metadata, review comments, and unified diff in one pass:

- `gh pr view --json number,title,body,comments`
- `gh pr diff`

Reuse these results for the entire review. Do not re-read the same files or
re-run the same diff piecemeal across turns -- the first fetch is authoritative.

## Reason about the changed hunks

The diff is the subject of the review. Work hunk by hunk: what does this change
do, is it correct, and what could break? Comments and context lines in the diff
are usually enough to judge it.

## Wider context only for a specific blocker

Open a file outside the diff only when a concrete concern forces it -- to confirm
a caller's behaviour, a type contract, or a test that should have changed. Before
opening it, name which hunk and which concern it serves. If you cannot tie the
read to a specific hunk, do not make it.

## Do not spider the call graph

This is the key override: do NOT use `codegraph_explore`, `codegraph_node`, or
`codegraph_trace` to map the area around the change. Call-graph traversal turns a
scoped review into an open-ended survey and reads far past the change under
review. A single targeted `codegraph_node` on one changed symbol -- it returns
that symbol's caller/callee trail -- is acceptable only when a specific hunk makes
you suspect a caller broke; a blanket "let me explore the impact" is not.
