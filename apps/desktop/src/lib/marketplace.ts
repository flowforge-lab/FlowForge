// FE-owned marketplace types for the Skills → Marketplace sub-tab (SET.5). There
// is no backend/ts-rs binding for a remote skill catalog yet, so this type lives
// in `lib/` (mirroring the SET.4 `ControlConfig` precedent in `lib/control.ts`)
// and is `import type`'d into `ipc.ts`. `bindings/` stays untouched.

/** A skill as surfaced by the (mock) marketplace search. */
export interface MarketplaceSkill {
  /** Unique skill identifier, e.g. `docx-author`. */
  name: string;
  description: string;
  version: string;
  /** Publisher handle shown on the result card. */
  author: string;
  keywords: string[];
  /** Rough popularity signal used for ordering and display. */
  installs: number;
}
