# AGENTS.md — working agreement for AI agents in this repo

This is the behavioural contract for an AI agent working in the FlowForge
workspace. `CONTRIBUTING.md` covers how humans submit changes (tool
registration, safety tiers, exit-code contract, code map) — read it too and do
not duplicate it here. This file covers the two things it does not: **which tool
to reach for**, and **the local-environment traps every agent hits**.

## Tool-first defaults

Reach for the first-class tool before shelling out. `bash`/`gh`/raw `git` are
the fallback, not the default.

- **GitHub → the `github` tool** (pr_view, pr_list, pr_reviews,
  pr_review_comments, pr_checks, pr_create, pr_merge, pr_review, pr_comment,
  pr_request_review, issue_view, issue_list, issue_create, issue_edit,
  issue_comment, push). Not `bash gh ...`.
- **Git queries + mutations → the `git` tool** (status, diff, log, show, branch,
  commit). Not `bash git ...`. Whole branch→commit→push→draft-PR flow →
  `propose_pr`.
- **Reading code → `codegraph_explore`** before grep/glob/read (run `tool_search`
  first — like `test_runner` it is not in the default set). It is the only tool
  codegraph advertises: upstream deliberately unlists the narrower ones
  (`codegraph_callers`, `_callees`, `_impact`, `_search`, `_node`, `_files`,
  `_status`) unless `CODEGRAPH_MCP_TOOLS` re-enables them, because what they
  returned now arrives inline. Do not call one you have not seen in your own tool
  list — send a second focused `explore` instead of falling back to grep.
- **Running tests → `test_runner`** (run `tool_search` first — it is not in the
  default tool set). **Compile-checking → `diagnostics`** (`cargo check`).
- **Files → view / edit / apply_patch / write**, and grep / glob / tree — not
  `bash cat / sed / find / rg`.

Fall back to shell only when the tool genuinely cannot do it:

- Reading a blob at an arbitrary ref: `git show <ref>:<file>`, or the PR diff via
  `gh api ... -H "Accept: application/vnd.github.diff"`. The `git` tool only
  reads the working tree.
- GitHub operations the native actions do not cover: fetching a specific comment
  id, timeline events, closed-PR search, `gh issue close`.
- Git mutations the `git` tool does not cover: rebase, and commit/branch in
  combinations it cannot express. (A plain push is `github push`; a new branch
  and a commit are `git branch` / `git commit`.)
- Compound pipelines, e.g. `TMPDIR=/tmp ./scripts/test.sh`.

## Local-environment traps

- **Run the full suite as `TMPDIR=/tmp ./scripts/test.sh`.** The default in-repo
  `TMPDIR` makes `tempfile` dirs land inside the tree, so `ignore`/`git` walk up
  to the repo's `.gitignore`/`.git` and fixtures get filtered out — a large batch
  of false failures (glob/grep/tree/git/`respects_gitignore`/`not_a_repo`
  tests). CI runs with `TMPDIR=/tmp`, which is why it stays green.
- **Never use `cargo test --workspace` for the full run.** `flowforge-desktop`'s
  `crate-type` includes staticlib/cdylib, so some Cargo versions silently skip
  its test binary (#1124). `scripts/test.sh` (nextest, process-per-test) is the
  blessed entry point and mirrors CI exactly.
- **Verify comments / docs / small edits with `cargo check` (~9s), not a release
  build (~41s).** Cargo re-fingerprints by file content, so touching a
  foundational crate cascades a rebuild downstream regardless — drop the profile
  (`check`), do not expect it to skip.
- **Mutation-test in a leaf crate, not the `flowforge-desktop` monolith.**
  Mutating a leaf-crate source recompiles in ~25s; mutating a desktop source
  triggers a full desktop rebuild + link (130–489s). Keep testable logic in leaf
  crates; pin desktop wiring with the smallest assertion; batch multiple desktop
  mutations rather than edit-run-revert.
- **Wait on CI / slow suites with `test_runner background: true` + an observer.**
  Do not blind-`sleep`-poll `gh pr checks`.

## Verification gate

- **Inner loop:** the smallest `cargo nextest -p <crate> -E 'test(/name/)'`
  filter for the affected tests. The full gate runs once, before push.
- **Before push:** `cargo fmt --all --check`, then
  `cargo clippy --workspace --all-targets -- -D warnings`, then
  `TMPDIR=/tmp ./scripts/test.sh`. The `-D warnings` flag is what CI enforces —
  without it, real `dead_code`/unused errors hide among pre-existing ts-rs
  warnings and slip to CI.
- **Frontend** (`apps/desktop`): `pnpm typecheck && pnpm lint && pnpm
  format:check && pnpm build`. `format:check` is a CI step — run it, not just a
  self-chosen subset. Node/pnpm here are managed by mise
  (`~/.local/share/mise/shims`), not homebrew.
- **Repo-wide** (from the root): `pnpm check:control-chars`. Rejects control
  characters in tracked source — a stray one makes git treat the file as binary
  and hides the change from review entirely (#1185). Runs in CI, takes ~0.2s.

## Anti-over-engineering

Match the change to the problem: the smallest diff that solves it, trusting the
type-system and framework guarantees you already have. The principle and its
precedents live in `PRINCIPLES.md` Pillar 4 ("Match the change to the problem"),
alongside "Find it before you write it" — this is a philosophy tenet, not a
local-environment trap, so it is stated there rather than duplicated here.

## PR discipline

- **Rebase onto latest `main` before enabling auto-merge / squash-merge.**
- **Review the PR's ref, not the local working tree** — judge from the branch
  blob (`git show <ref>:<path>` or the PR diff), since the checkout may be on a
  different branch.
