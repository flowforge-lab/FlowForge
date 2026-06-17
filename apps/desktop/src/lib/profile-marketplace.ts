// FE-owned marketplace type for the Profiles → Marketplace sub-tab (SET.7). There
// is no backend/ts-rs binding for a remote profile catalog yet, so this type lives
// in `lib/` (mirroring SET.5's `lib/marketplace.ts`) and is `import type`'d into
// `ipc.ts`. `bindings/` stays untouched.

/** A profile (phenotype) as surfaced by the (mock) marketplace search. */
export interface MarketplaceProfile {
  /** Unique profile identifier, e.g. `data-science`. */
  id: string;
  name: string;
  description: string;
  /** Number of skills the profile activates. */
  skillCount: number;
  /** Publisher handle shown on the result card. */
  author: string;
  /** Rough popularity signal used for ordering and display. */
  installs: number;
}
