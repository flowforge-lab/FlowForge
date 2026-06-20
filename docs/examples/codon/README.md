# Codon (码子) — programming phenotype

A codon is the unit of the genetic code. In FlowForge's genotype/phenotype model,
installed skills are the latent genes and a phenotype is the expressed set. `codon`
is a programming phenotype: an engineering-discipline persona plus the `codegraph`
skill as its DNA, so switching to it brings code-aware navigation along.

**FlowForge ships Codon built in.** On first run it seeds `phenos/codon.toml` and
`skills/codegraph/SKILL.md` into `~/.flowforge/` if they are absent, so the
phenotype and its skill are present without any manual copy. The files in this
directory are the single source of truth those seeded copies are bundled from.

## What's here

- `phenos/codon.toml` — the phenotype: persona, `skills = ["codegraph"]`, a raised
  `max_iterations` for long edit/build/test/fix loops.
- `skills/codegraph/SKILL.md` — the codegraph skill, declaring `mcp: ["codegraph"]`.

## Setup

Seeding handles the FlowForge-authored content. You still need codegraph's
**third-party** binary and its MCP server entry — FlowForge does not (and cannot)
install those for you.

### 1. Install codegraph and index your project

```sh
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh
# open a new terminal so `codegraph` is on PATH, then, inside your project:
codegraph init
```

`codegraph install` auto-configures known agents (Claude Code, Cursor, …) but not
FlowForge — FlowForge wires the MCP server for you (step 2).

### 2. Enable the seeded codegraph MCP server

On first run FlowForge seeds a **disabled** `codegraph` entry into
`~/.flowforge/mcp.json`:

```json
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp"],
      "disabled": true
    }
  }
}
```

It ships disabled because the binary (step 1) may not be installed yet. Once it is,
**enable it in Settings → MCP** (or flip `"disabled": false`). The seed never
overwrites a `codegraph` entry you wrote yourself.

### 3. Select it

Pick the **codon** phenotype in the composer's phenotype switcher, or run the CLI
with `--pheno codon`. On activation FlowForge warns if the `codegraph` server is
missing from `mcp.json` or not running — the persona still loads and grep/glob
fallbacks work, but codegraph's tools won't be available until you complete step 2.

## Customizing

The seed never clobbers an existing file, so you can edit your copies freely:

- Tune the persona or bump `max_iterations` in `~/.flowforge/phenos/codon.toml`.
- Author your own variant by copying these files under a new name:

  ```sh
  cp docs/examples/codon/phenos/codon.toml ~/.flowforge/phenos/my-codon.toml
  cp -r docs/examples/codon/skills/codegraph ~/.flowforge/skills/codegraph
  ```

Deleting a seeded file makes it reappear on the next launch (seed-if-absent); a
permanent removal is part of the Phase 2 follow-up below.

## Notes

- `model` is intentionally unset in `codon.toml`; pin a capable model once your
  provider connection is configured.
- FlowForge seeds the codegraph `mcp.json` entry **disabled** on first run, then
  *requires* the server at activation (require + warn). Injecting a full server
  definition from any skill's DNA on activation (the general "zero-step DNA" case,
  beyond Codon's seeded entry) is tracked in #306.
- **Bundling.** Phase 1 (seeded on first run, write-if-absent) ships today. Phase 2
  — compiling Codon in as a true built-in that survives deletion or a read-only
  home, like the `default` phenotype — is tracked in
  [#306](https://github.com/flowforge-lab/FlowForge/issues/306).
