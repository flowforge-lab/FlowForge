import { describe, expect, it } from "vitest";

import {
  keyStatusLabel,
  searchBackendMeta,
  SEARCH_BACKENDS,
} from "@/lib/search";

describe("search backend registry", () => {
  it("lists SearXNG first and marks hosted backends as key-gated", () => {
    expect(SEARCH_BACKENDS[0]?.id).toBe("searxNg");
    expect(searchBackendMeta("searxNg").requiresKey).toBe(false);
    expect(searchBackendMeta("brave").requiresKey).toBe(true);
    expect(searchBackendMeta("openAiCompatible").requiresKey).toBe(true);
  });

  it("only SearXNG exposes a base URL field", () => {
    expect(searchBackendMeta("searxNg").showBaseUrl).toBe(true);
    expect(searchBackendMeta("brave").showBaseUrl).toBe(false);
  });
});

describe("keyStatusLabel", () => {
  it("never embeds secret material", () => {
    expect(keyStatusLabel(true)).toBe("API key set");
    expect(keyStatusLabel(false)).toBe("API key not set");
    expect(keyStatusLabel(true)).not.toMatch(/sk-/);
  });
});
