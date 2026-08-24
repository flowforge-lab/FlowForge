import { describe, expect, it } from "vitest";
import { filterModels, FILTER_THRESHOLD } from "@/lib/model-filter";

/** A realistic slice of what one large aggregator returns — near-identical ids
 *  sharing long prefixes, which is what makes the unfiltered list unusable. */
const MODELS = [
  "openai/gpt-5.6",
  "openai/gpt-5.6-sol:batch",
  "openai/gpt-4o-mini",
  "x-ai/grok-4.5",
  "~x-ai/grok-latest",
  "poolside/laguna-xs-2.1",
  "poolside/laguna-xs-2.1:free",
  "anthropic/claude-sonnet-5",
  "anthropic/claude-opus-5",
  "tencent/hy3",
];

function models(query: string, from = MODELS): string[] {
  return filterModels(query, from);
}

describe("filterModels (#1301)", () => {
  it("narrows to a model family from a few characters", () => {
    // The issue's own example: typing `gpt5` should surface the gpt-5 variants
    // and nothing else, without the user typing the `-` or the vendor prefix.
    expect(models("gpt5")).toEqual([
      "openai/gpt-5.6",
      "openai/gpt-5.6-sol:batch",
    ]);
  });

  it("matches a subsequence, so a partial vendor+model works", () => {
    expect(models("son5")).toContain("anthropic/claude-sonnet-5");
    expect(models("son5")).not.toContain("anthropic/claude-opus-5");
  });

  it("is case-insensitive", () => {
    expect(models("GROK")).toEqual(models("grok"));
    expect(models("GROK")).toHaveLength(2);
  });

  it("ranks the tighter match first", () => {
    // `laguna-xs-2.1` is a contiguous run in both, but the shorter id has no
    // trailing `:free`, so the two must come back in a defined order rather
    // than the provider's.
    expect(models("laguna")).toEqual([
      "poolside/laguna-xs-2.1",
      "poolside/laguna-xs-2.1:free",
    ]);
  });

  it("breaks score ties alphabetically, not by provider order", () => {
    const scrambled = [...MODELS].reverse();
    // Same set either way: ordering must not depend on input order once a
    // query is present.
    expect(models("claude", scrambled)).toEqual(models("claude"));
  });

  it("returns nothing for a query that matches no model", () => {
    expect(models("gtp5")).toEqual([]);
  });

  it("keeps the provider's order for an empty or blank query", () => {
    expect(models("")).toEqual(MODELS);
    expect(models("   ")).toEqual(MODELS);
  });

  it("filters only the provider's own catalog", () => {
    // The picker keeps its connection → models shape (#1301 review): each
    // provider's submenu filters its own list, so nothing here reaches across
    // connections.
    const ollama = ["llama3.2", "qwen2.5", "mistral"];
    expect(models("gpt5", ollama)).toEqual([]);
    expect(models("qwen", ollama)).toEqual(["qwen2.5"]);
  });

  it("exposes a threshold small enough that a local provider skips the box", () => {
    // Ollama lists a handful of models; its submenu must not sprout a filter.
    expect(FILTER_THRESHOLD).toBeGreaterThan(5);
  });
});
