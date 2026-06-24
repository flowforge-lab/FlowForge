# apply_patch — deferred follow-ups

Captured from Abid's review of #233 (the `apply_patch` tool). All three are
**non-blocking and fail-safe** today: the tool aborts and writes nothing in each
case. They were deferred from the #233 merge so they are not lost when #219
closes.

Status legend: `done` = landed as an interim fix in this pass; `open` = tracked
for the matching/atomicity pass below.

## 1. Commit phase is not atomic across files  — `done` (doc); `open` (mechanism)

Validation (Phase 1) is fully atomic: any context mismatch aborts *before* a byte
is written. But if write #1 succeeds and write #2 fails mid-commit (permissions,
disk full), #1 is already on disk with no rollback.

- **Done now**: narrowed the "all-or-nothing" claim in the module doc comment and
  the tool `description()` to the *validation* phase, so the contract is not
  overstated.
- **Open**: true multi-file atomicity via **temp-write + rename/swap** commit —
  stage every new file content to a temp path, then `rename(2)`/swap them into
  place (and unlink deletes) only after all staging succeeds, so a mid-commit I/O
  error leaves the original tree intact.

## 2. No-trailing-newline final line fails to match  — `open`

Reconstructed `old`/`new` lines each carry a trailing `\n` (see
`parse_update_body`), so a hunk touching the last line of a file with no final
newline won't find its `old` block → "context not found". Fails safe (aborts).

Folded into the fuzzy/strict-matching pass below: normalize the trailing newline
before matching (e.g. also try the `old` block with its final `\n` stripped, or
canonicalize both the file and `old` to a single trailing-newline convention
before comparison and restore the original convention on write).

## 3. Pure-addition hunk gives a confusing error  — `done` (message); `open` (anchoring)

A hunk with only `+` lines (no context/removals) has an empty `old`, so
`matches("")` counts every position → "context is ambiguous (N matches)".
Rejecting an unanchored insert is correct.

- **Done now**: reject the empty-`old` case up front with a targeted message —
  *"hunk N has no context line to anchor the addition; prepend a context line
  (` `) before the `+` line(s)"* — instead of the misleading N-match ambiguity.
- **Open**: a fuzzy pass may relax this by anchoring such a hunk to the file
  head/tail or to adjacent context, instead of rejecting outright.

## Future pass: fuzzy / strict matching for apply_patch

Items 2 & 3 fold naturally into a single fuzzy/strict-matching pass for
`apply_patch`, mirroring the leading-whitespace-tolerant fuzzy fallback already
used by `edit`. Likely shape:

- Canonicalize trailing newlines on both the file and the reconstructed `old`
  block before the uniqueness check (item 2).
- Optionally anchor pure-addition hunks to head/tail or the boundary of the
  nearest context line (item 3), rather than hard-rejecting.
- Consider a configurable strict-vs-fuzzy mode so exact-match guarantees stay
  available where the model needs determinism.

Keep every relaxation fail-safe: if a relaxed match is not unique, fall back to
the existing "context is ambiguous; add more context lines" error.
