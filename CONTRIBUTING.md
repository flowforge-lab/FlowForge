# Contributing to FlowForge

Thanks for your interest in FlowForge! 🔗🧠

## Before You Start

All contributions — code, docs, and design — follow our
**[Engineering Principles](./PRINCIPLES.md)**. Please read the charter first;
it is short, opinionated, and binding. Every pull request is reviewed against
its four pillars:

1. **Flow for the User, First**
2. **Efficiency: Footprint & Latency**
3. **Adaptive & Migratable**
4. **Code the Zen Way**

## Workflow

1. **Open an issue** describing the change before large work, so we can align early.
2. **Branch** from `main`.
3. **Implement** — match existing patterns, keep modules flat, handle errors explicitly.
4. **Verify** before opening a PR:
   - `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
   - `cargo test --workspace`
   - `pnpm typecheck && pnpm lint && pnpm test`
5. **Open a PR** with a clear *why* in the description. If you can't explain the
   implementation in a few sentences, reconsider the design (Pillar 4).

## Quick Start

See the [Development section in the README](./README.md#-development) for setup.

## Questions

Open a discussion or an issue. We'd rather talk early than guess later —
*in the face of ambiguity, refuse the temptation to guess.*
