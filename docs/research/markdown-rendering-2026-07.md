# Markdown rendering: keep, improve, or migrate? (2026-07)

**Audience:** Abid (FE owner). **Trigger:** Tony's subjective sense that chat-message
rendering looks worse than a competing app ("Aki").

## TL;DR

**Don't migrate to Streamdown. Do one small, low-risk swap: replace `rehype-highlight`
(highlight.js) with Shiki for code syntax highlighting.** That's the one place our current
stack is objectively behind, it's a component-level change (not an architecture change),
and it doesn't touch the two things we've already built custom, hard-won solutions for:
the streaming block-splitting optimizer (#844/#104) and the no-raw-HTML security posture
(#1129). Everything else in the "Aki renders better" complaint is unverified feeling —
see the honesty section below before spending more engineering time on it.

## What we have today

`apps/desktop/src/components/markdown.tsx`:
`react-markdown` (^10.1.0) + `remark-gfm` + `remark-math` + a hand-rolled
`remark-backslash-math` plugin (for `\(...\)`/`\[...\]` LaTeX delimiters some model
families emit) + `rehype-highlight` (highlight.js) + `rehype-katex`. No `rehype-raw`
— raw HTML from the model is never rendered (deliberate XSS defense against untrusted
LLM output). Streaming performance is handled by a hand-rolled block-splitter
(`lib/markdown-blocks.ts`, #844) that memoizes "closed" blocks and only re-parses a
growing "open tail," avoiding O(n^2) re-parse cost per token (#104). Custom `<a>`
override enforces a scheme allowlist through Tauri's OS-browser opener (#1129); custom
`<code>`/`<pre>` give Copy + "open in split panel" (#11); KaTeX renders live during
streaming (#1102), highlight.js only runs once at turn end.

## Streamdown — what it actually is

Per its own docs (streamdown.ai, github.com/vercel/streamdown, npm): **"a drop-in
replacement for react-markdown, designed for AI-powered streaming."** It is *built on*
react-markdown, remark, and rehype — not a different architecture. Its headline
capability is handling **unterminated/incomplete Markdown tokens mid-stream** (e.g. an
opened `**bold` or a code fence that hasn't closed yet) so they render sensibly instead
of as broken syntax, plus a "streaming caret" affordance and built-in Tailwind typography.

Key facts, cited to Streamdown's own docs:

- **Plugin surface**: math, code (Shiki-based), and mermaid ship as separate optional
  packages (`@streamdown/math`, `@streamdown/code`, `@streamdown/mermaid`), and it
  supports custom component overrides and remark/rehype plugin injection
  (streamdown.ai/docs/components, streamdown.ai/docs/plugins/{code,math}). This means our
  custom `remark-backslash-math` plugin and our Copy/Split `<code>`/`<pre>` overrides
  and `<a>` scheme-allowlisting are *plausibly* portable — Streamdown doesn't replace
  react-markdown's plugin model, it wraps it.
- **Syntax highlighting**: uses **Shiki**, not highlight.js (streamdown.ai/docs/plugins/code:
  "Syntax highlighting for code blocks using Shiki").
- **Math**: KaTeX, same library we already use (streamdown.ai/docs/plugins/math).
- **Security**: ships a "Security" hardening layer described as protecting "against
  malicious Markdown" (streamdown.ai/docs/security) and a configurable **link-safety
  confirmation modal** for external links (streamdown.ai/docs/link-safety) — functionally
  the same *category* of defense as our current scheme-allowlist + OS-browser-opener
  approach, just a different mechanism (modal vs. allowlist). It does not appear to solve
  a problem we don't already have a working, shipped answer for.
- **Provenance/maintenance**: built by Vercel, powers the official AI SDK "AI Elements
  Message" component (npmjs.com/package/streamdown: "Streamdown powers the AI Elements
  Message component but can be installed as a standalone package"). Actively maintained,
  reputable maintainer, Apache-2.0 licensed.

**Important scoping point: Streamdown solves a *parsing-correctness* problem (don't
render broken syntax for half-arrived tokens), not the *performance* problem our
`markdown-blocks.ts` splitter solves (don't re-parse the whole growing message every
token).** These are different problems. Adopting Streamdown does not make #844 go away —
we'd likely still want the closed/open-tail split underneath it, or need to verify
Streamdown's internal incomplete-token handling is cheap enough at our message lengths
(some sessions run 100+ KB single messages — untested by Streamdown's own docs, no
benchmark published there). That's the one real open question a migration would have to
answer with our own profiling, not their marketing copy.

## Cheaper alternatives considered and rejected

- **`marked` / `markdown-it`** — lower-level Markdown-to-*string* parsers, not
  Markdown-to-React-elements. Both return HTML strings, so using either means either
  `dangerouslySetInnerHTML` on model-generated content (directly undermining our current
  no-raw-HTML XSS posture, since the whole point of that posture is that model output is
  untrusted) or hand-building a React-element renderer on top from scratch — a strictly
  bigger rewrite than adopting Streamdown, for less capability. Not worth it.
- **Shiki alone, keeping react-markdown** — same react-markdown/remark/rehype
  architecture we already have, one component swapped (`rehype-highlight` -> a
  Shiki-based rehype plugin, e.g. `@shikijs/rehype`, or a custom `<code>` renderer
  calling Shiki directly since we already have a custom `<pre>`/`<code>` override for
  Copy/Split). Shiki's own pitch (shiki.style): "TextMate grammar powered, same engine as
  your VS Code" vs. highlight.js's own, smaller grammar set — meaningfully higher
  highlighting fidelity for a coding-agent-heavy chat product, which is exactly the kind
  of thing "renders worse" complaints are usually actually about. This is the
  highest-value, lowest-risk change available: it's a one-file component swap, doesn't
  touch #844's streaming optimizer (we already skip highlighting during streaming and
  apply it once at turn end — same trigger point, just a different highlighter), doesn't
  touch #1129's security path, and has a much smaller blast radius than adopting a whole
  new markdown-rendering library.

## Migration risk, if you (Abid) want to evaluate Streamdown anyway

**Low risk / mechanical:**
- GFM tables/lists/task-lists — built in, matches remark-gfm behavior.
- KaTeX math — same underlying library, config should port directly.

**Medium risk / needs verification, not rewrite:**
- `remark-backslash-math` — needs confirming Streamdown's plugin injection point accepts
  arbitrary remark plugins the same way react-markdown's `remarkPlugins` prop does
  (docs say yes, component-level, but we haven't proven it against our specific plugin).
- Copy button / "open in split panel" on code blocks, `<a>` scheme allowlist — Streamdown's
  own component-override doc (streamdown.ai/docs/components) exists specifically for this
  use case, so it's supported, but every override needs to be re-verified against
  Streamdown's actual component prop shape (likely different prop names than
  react-markdown's `components` map) — an afternoon of porting + testing per override,
  not a rewrite.

**Bigger lift / the actual go/no-go question:**
- Whether Streamdown's incomplete-token handling is fast enough at our message-length
  extremes to let us **delete** `markdown-blocks.ts` (#844) entirely, or whether we'd
  still need to keep our own closed/open-tail split running underneath it. This is not
  answered anywhere in Streamdown's docs — it needs a profiling spike against a real
  100+ KB streaming FlowForge message before any migration decision, not before a Shiki
  swap.

## Honesty note: "renders worse than Aki" is a feeling, not a finding

I could not confirm from static inspection of Aki's bundled JS which library its chat
surface actually uses — the only markdown-library string found across its five app
bundles was `marked`, in the *kanban* sub-app bundle, not confirmed to be the chat/message
renderer. Treat that as a stray data point, not evidence Aki's chat is "better because of
library X." Before spending more engineering time chasing this, turn the vague feeling
into checkable comparisons — put FlowForge and Aki side by side on the same content and
look specifically at:

1. **Syntax highlighting theme fidelity** — do colors/tokens match what you'd see in VS
   Code, or does something look "flat"/wrong (this is the concrete case Shiki fixes).
2. **Code block chrome** — padding, border radius, header bar, button placement/hover
   states.
3. **Table rendering** — column alignment, borders, header weight.
4. **Streaming jank** — does text visibly "pop"/reflow/flicker as tokens arrive, versus
   arriving smoothly.

Whichever of these actually shows a visible gap tells you whether the fix is the Shiki
swap, a CSS/Tailwind styling pass, or something else — not a wholesale renderer migration.

## Next action

Swap `rehype-highlight` for a Shiki-based highlighter in `markdown.tsx` (keeping
everything else — react-markdown, remark-gfm/math, the custom backslash-math plugin, the
#844 streaming splitter, and the #1129 link allowlist — unchanged), then do the four-point
side-by-side comparison above against Aki to confirm whether that alone closes the
perceived gap before considering anything bigger.
