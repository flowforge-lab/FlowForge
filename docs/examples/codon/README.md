# Codon (码子) — programming phenotype

A codon is the unit of the genetic code. In FlowForge's genotype/phenotype model,
installed skills are the latent genes and a phenotype is the expressed set. `codon`
is a programming phenotype: an engineering-discipline persona plus the `codegraph`
skill as its DNA, so switching to it brings code-aware navigation along.

This directory holds version-controlled copies of the content; FlowForge reads its
phenotypes and skills from `~/.flowforge/`, so install them as below.

## What's here

- `phenos/codon.toml` — the phenotype: persona, `skills = ["codegraph"]`, a raised
  `max_iterations` for long edit/build/test/fix loops.
- `skills/codegraph/SKILL.md` — the codegraph skill, declaring `mcp: ["codegraph"]`.

## Install

### 1. Install codegraph and index your project

```sh
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh
# open a new terminal so `codegraph` is on PATH, then, inside your project:
codegraph init
```

`codegraph install` auto-configures known agents (Claude Code, Cursor, …) but not
FlowForge — wire it up manually in step 2.

### 2. Add the codegraph MCP server to `~/.flowforge/mcp.json`

```json
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp"]
    }
  }
}
```

The server id `codegraph` must match the `mcp` entry in the skill's frontmatter.

### 3. Install the codegraph skill

Install from this directory (it is copied into `~/.flowforge/skills/`):

```sh
# via the FlowForge skill installer (desktop Settings → Skills → Install, or CLI)
install_skill docs/examples/codon/skills/codegraph
# …or copy it by hand:
cp -r docs/examples/codon/skills/codegraph ~/.flowforge/skills/codegraph
```

### 4. Install the phenotype

```sh
mkdir -p ~/.flowforge/phenos
cp docs/examples/codon/phenos/codon.toml ~/.flowforge/phenos/codon.toml
```

### 5. Select it

Pick the **codon** phenotype in the composer's phenotype switcher, or run the CLI
with `--pheno codon`. On activation FlowForge warns if the `codegraph` server is
missing from `mcp.json` or not running — the persona still loads and grep/glob
fallbacks work, but codegraph's tools won't be available until you complete step 2.

## Notes

- `model` is intentionally unset in `codon.toml`; pin a capable model once your
  provider connection is configured.
- FlowForge currently *requires* the codegraph server to be present (require +
  warn); it does not inject it into `mcp.json` on activation. Auto-injection
  ("zero-step DNA") is a tracked follow-up.
