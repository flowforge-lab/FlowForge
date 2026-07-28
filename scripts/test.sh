#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Blessed local test entry point for the FlowForge workspace.
#
# Use this instead of `cargo test --workspace`, which may silently skip the
# flowforge-desktop test binary on some Cargo versions because its crate-type
# includes staticlib/cdylib (#1124).
#
# This script matches exactly what CI runs (ci.yml) so local and CI never diverge.

# 1. Run the full workspace test suite with nextest's process-per-test scheduler.
#    --no-fail-fast is the default via .config/nextest.toml, but we pass it
#    explicitly so the intent is obvious even if the config file is overridden.
cargo nextest run --workspace --no-fail-fast

# 2. Run doctests separately — nextest does not support them.
cargo test --workspace --doc
