import type { SearchBackend } from "@/bindings/SearchBackend";

/** Display metadata for a selectable web-search backend (#84). */
export type SearchBackendMeta = {
  id: SearchBackend;
  label: string;
  description: string;
  /** Hosted APIs that need an OS-keychain secret (#8). */
  requiresKey: boolean;
  /** Whether the settings panel exposes a base-URL field for this backend. */
  showBaseUrl: boolean;
};

/** Backends exposed in settings — order matches the IPC contract. */
export const SEARCH_BACKENDS: SearchBackendMeta[] = [
  {
    id: "searxNg",
    label: "SearXNG",
    description: "Self-hosted, keyless JSON API",
    requiresKey: false,
    showBaseUrl: true,
  },
  {
    id: "brave",
    label: "Brave Search",
    description: "Hosted search API",
    requiresKey: true,
    showBaseUrl: false,
  },
  {
    id: "openAiCompatible",
    label: "OpenAI-compatible",
    description: "Hosted search endpoint",
    requiresKey: true,
    showBaseUrl: false,
  },
];

export function searchBackendMeta(id: SearchBackend): SearchBackendMeta {
  const meta = SEARCH_BACKENDS.find((b) => b.id === id);
  if (!meta) {
    throw new Error(`unknown search backend: ${id}`);
  }
  return meta;
}

/** Human-readable key status — never includes secret material (#8 rules). */
export function keyStatusLabel(hasKey: boolean): string {
  return hasKey ? "API key set" : "API key not set";
}
