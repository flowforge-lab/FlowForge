# Model capability overrides

FlowForge decides whether a model accepts image attachments, and how large its
context window is, by matching the model id against a bundled table. That table
is a best-effort map of the models we could probe — it will be wrong about
something eventually, especially for a gateway that renames models or a provider
that ships a new family faster than we can test it.

When it is wrong, you can correct it yourself without waiting for a release.

## The override file

Create `model-specs.json` in FlowForge's config directory:

| OS | Path |
| --- | --- |
| macOS | `~/Library/Application Support/flowforge/model-specs.json` |
| Linux | `~/.config/flowforge/model-specs.json` |
| Windows | `%APPDATA%\flowforge\model-specs.json` |

```json
{
  "rules": [
    {
      "model": "my-gateway/vision-preview",
      "provider": "openai",
      "supports_vision": true
    },
    {
      "model": "some-long-context-model",
      "context_window": 262144
    }
  ]
}
```

Restart FlowForge after editing — the file is read once at startup.

## How matching works

- **`model` is a substring match**, not an exact id. A rule for `glm` matches
  `glm-4.5-air`, so order matters.
- **First match wins**, and **your rules are consulted before the bundled ones**.
  That is what makes this an override: a rule of yours that matches shadows any
  bundled rule for the same model.
- Within your own file, put the most specific rule first (`glm-4.5-air` before
  `glm`), for the same reason.
- **`provider` scopes a rule** to one provider kind (`openai`, `bedrock`,
  `anthropic`, `ollama`, …). The same model substring can mean different things on
  different providers, so a vision rule without a `provider` applies everywhere —
  usually not what you want. Omit it only for `context_window`, which is
  provider-agnostic.

Every field is optional except `model`. A rule that sets only `context_window`
leaves the vision verdict to the bundled table, and vice versa.

## Turning attachments on for a model we got wrong

This is the common case: you know a model takes images, but FlowForge greys out
the attach button.

```json
{ "rules": [{ "model": "deepseek-ai/DeepSeek-V3.2", "provider": "openai", "supports_vision": true }] }
```

The gate is **fail-closed**: an unknown model is treated as *no vision*, because
sending an image to a model that can't read it produces a provider error rather
than a graceful degrade. So the fix is always to add a rule, never to remove one.

You can also correct the opposite direction — set `"supports_vision": false` for a
model whose vision support is nominal but broken in practice.

## If the file has a mistake in it

A `model-specs.json` that is unreadable or isn't valid JSON is **renamed** to
`model-specs.corrupt-<timestamp>.json` beside itself, and FlowForge starts with
the bundled defaults. Your file is never deleted or silently rewritten, so a typo
costs you a restart, not your edits — check the renamed file to see what it
choked on.

Failures are logged at `error` level, so the log will name the file and the parse
error. Logs live at `<data_dir>/logs/flowforge.log` (on macOS,
`~/Library/Application Support/flowforge/logs/flowforge.log`), rotated daily.

## Caveats

- The context window here is the **model's** window, not the fraction of it
  FlowForge will fill. The soft budget that triggers compaction is a separate
  setting.
- Overriding `supports_vision` to `true` on a model that genuinely can't accept
  images doesn't make it work — it just moves the failure from a greyed-out
  button to a provider error mid-turn.
- These rules key off the model **id**, so a gateway that serves different
  models under one id can't be described precisely. Prefer a provider-scoped rule
  as narrow as you can make it.

## For contributors

If a correction here would help everyone, send the rule to
`crates/ff-core/src/model-specs.default.json` as well — that is the bundled table,
and it is deliberately narrow: adding a model there un-gates attachments app-wide
for every connection whose model id matches.

Note that `ff_core::model_supports_vision` reads the **bundled table only** — it
cannot see this file, because `ff-core` performs no I/O. Application paths must
use `ff_llm::model_supports_vision`, which reads the merged set. Getting that
wrong is silent (the override simply stops working), so a test fails the build if
anything outside `ff-core` calls the bundled-only lookup. See #1137.
