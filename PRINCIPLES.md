# FlowForge Engineering Principles

> **What this is.** This document is the charter for FlowForge. It binds everyone
> who commits to this repository — maintainers, contributors, and AI agents alike.
> It is intentionally short and opinionated. When a decision is unclear, this is
> the document you reach for.
>
> **How it changes.** This charter changes only through a pull request reviewed and
> approved by maintainers. It is a constitution, not a scratchpad. See
> [Amending This Document](#amending-this-document).

---

## The Four Pillars

1. **Flow for the User, First** — the user's flow state is sacred.
2. **Efficiency: Footprint & Latency** — fast and light by default.
3. **Adaptive & Migratable** — meet users where they already are.
4. **Code the Zen Way** — Python's Zen, applied to Rust + Tauri + TypeScript.

When pillars conflict, the **numbered order is the tiebreaker**: Flow (1) beats
Efficiency (2) beats Adaptive (3) beats Zen-style (4). The one exception is
*"practicality beats purity"* — if following the order produces something
absurd, stop and escalate to discussion rather than guessing.

---

## Pillar 1 — Flow for the User, First

The user should experience **flow** above all else. Every feature, every
interaction, every millisecond is measured against one question:

> **Does this add a step between the user's thought and their action?**

If the answer is yes, it is wrong until proven otherwise.

**In practice:**
- `<200ms` from invocation to interactive. Cold start is a feature, not a metric.
- **Keyboard-native.** Every action reachable without the mouse. `Cmd/Ctrl+K` is home.
- **Zero-modal.** No blocking dialogs that hijack attention. Prefer inline
  confirmation and undo-based design over "Are you sure?" popups.
- **No unsolicited interruptions.** The interface does not demand attention; it
  waits for it. Feedback is surfaced, never forced.
- **The agent is a partner, not a wall.** It remembers context so the user
  doesn't have to re-explain themselves.

**The test:** before merging, ask — *"Would this break someone who is deep in
thought?"* If yes, redesign.

---

## Pillar 2 — Efficiency: Footprint & Latency

Flow is impossible on a sluggish, bloated tool. Efficiency is how we earn
Pillar 1.

**In practice:**
- **Lazy by default, eager only when measured.** Defer work until it is needed.
  Background-load skills, providers, and indexes; never block the first paint.
- **Budgets, not vibes.** We hold ourselves to concrete numbers:
  - Cold start to interactive: **`<200ms`**
  - Idle memory footprint: keep it lean (target a low ceiling; measure, don't guess)
  - Binary size: minimize Tauri features; ship only what is used
- **Measure before optimizing.** No performance change lands without a
  before/after measurement. Benchmarks live in the repo, not in our heads.
- **Respect the user's machine.** Local-first means the user's CPU, RAM, and
  disk are *theirs*. Be a good guest.

**The test:** *"Did I measure this, or am I guessing?"* If guessing, measure first.

---

## Pillar 3 — Adaptive & Migratable

FlowForge does not ask users to abandon their existing tools and start over. We
**meet users where they are** and make migration first-class.

**In practice:**
- **Interop with existing AI assistants** — Aki, Hermes, OpenClaw, Claude, and
  others. We provide interfaces for users to migrate *in*, not walls to keep
  them *out*.
- **Migration interfaces are first-class, not afterthoughts.** Importing an
  existing skill set, memory store, or config is a designed feature with tests,
  not a one-off script.
- **Standards over lock-in.** Prefer open, portable formats and protocols:
  MCP for tools, OpenAI-compatible APIs where practical, portable skill/memory
  formats. The user's data and workflows are *theirs* and must remain portable
  *out* as easily as they came *in*.

**The test:** *"Can a user bring their existing setup in — and take everything
back out — without losing anything?"*

---

## Pillar 4 — Code the Zen Way

We adopt the Zen of Python as our engineering aesthetic, translated to a
Rust + Tauri + TypeScript codebase. The full text follows, with a gloss on how
each line binds our code.

```
The Zen of Python, by Tim Peters

Beautiful is better than ugly.
Explicit is better than implicit.
Simple is better than complex.
Complex is better than complicated.
Flat is better than nested.
Sparse is better than dense.
Readability counts.
Special cases aren't special enough to break the rules.
Although practicality beats purity.
Errors should never pass silently.
Unless explicitly silenced.
In the face of ambiguity, refuse the temptation to guess.
There should be one-- and preferably only one --obvious way to do it.
Although that way may not be obvious at first unless you're Dutch.
Now is better than never.
Although never is often better than *right* now.
If the implementation is hard to explain, it's a bad idea.
If the implementation is easy to explain, it may be a good idea.
Namespaces are one honking great idea -- let's do more of those!
```

**How each line applies here:**

| Koan | In FlowForge |
|------|--------------|
| Beautiful is better than ugly | `cargo fmt` + `clippy`, `prettier` + `eslint` are non-negotiable. CI rejects ugly. |
| Explicit is better than implicit | No magic globals, no hidden side effects. Pass dependencies in; don't reach out. |
| Simple is better than complex | Extend an existing pattern before inventing a new abstraction. YAGNI. |
| Complex is better than complicated | When complexity is unavoidable, contain it behind a clear, well-named boundary. |
| Flat is better than nested | Shallow module trees. `docs/PRINCIPLES.md` only once `docs/` earns its keep. |
| Sparse is better than dense | One idea per function. Let code breathe. |
| Readability counts | Code is read far more than written. Optimize for the next reader. |
| Special cases aren't special enough… | Resist `if user == "tony"` style branches. Generalize or don't. |
| …Although practicality beats purity | This is also our pillar-conflict escape hatch. Ship the pragmatic thing. |
| Errors should never pass silently | No `.unwrap()`/`.expect()` on fallible prod paths. No swallowed `Result`/`catch {}`. |
| Unless explicitly silenced | If an error is truly ignorable, say so in code with a reason: `let _ = …; // why`. |
| In the face of ambiguity, refuse to guess | Don't infer user intent silently. Ask, or surface the ambiguity. |
| One — and preferably only one — obvious way | One config path. One state store (Zustand). One way to register a tool. |
| …unless you're Dutch | A wink: sometimes the right way is non-obvious. Document it when it is. |
| Now is better than never | Ship the vertical slice. Don't gold-plate. |
| Although never is often better than *right* now | Don't ship the band-aid. Root cause over quick fix. |
| If the implementation is hard to explain, it's a bad idea | If you can't explain it in a PR description, redesign it. |
| If easy to explain, it may be a good idea | Simple explanations are a signal of good design. |
| Namespaces are one honking great idea | Crate boundaries (`ff-*`) are our namespaces. Keep them clean and purposeful. |

**The test:** *"Could I explain this implementation in three sentences to a new
contributor?"* If not, it is probably a bad idea.

---

## How These Apply in Review

Every pull request — human or agent authored — is checked against this charter:

- [ ] **Pillar 1** — Does it preserve user flow? No new step between thought and action?
- [ ] **Pillar 2** — Was any performance-relevant change measured (before/after)?
- [ ] **Pillar 3** — Does it keep data/workflows portable in *and* out?
- [ ] **Pillar 4** — Does it pass fmt/clippy/lint, handle errors explicitly, and
      stay easy to explain?

When pillars conflict, apply the numbered priority (1 > 2 > 3 > 4). When the
priority produces an absurd result, invoke *"practicality beats purity"* and
escalate to discussion — **refuse the temptation to guess.**

---

## Amending This Document

This charter is deliberately stable. To change it:

1. Open a pull request that edits this file.
2. Explain the *why* in the PR description.
3. Obtain maintainer approval.

Charter changes are not casual edits. They are deliberate, reviewed, and rare.
