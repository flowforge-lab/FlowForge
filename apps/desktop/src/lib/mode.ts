// Presentation metadata for the agent modes (#266, RFC 0011). Kept React-free so
// it's unit-testable and shared by the composer pill and the Settings control.
// Colour coding: Plan = blue/calm, Act = green/go, Auto = amber/caution.

import type { Mode } from "@/bindings";

export interface ModeMeta {
  label: string;
  /** One-liner shown on hover / under the Settings control. */
  description: string;
  /** Tailwind classes for the pill (border + tint + text), light & dark. */
  pillClass: string;
  /** Tailwind classes for the status dot. */
  dotClass: string;
}

// Descriptions state the *intent* of each mode, not an enforced guarantee: until
// the backend mode-IPC seam lands (#281 follow-up), this selection is display-only
// and does not yet gate the turn (see NOT_ENFORCED_NOTE). Light tints bumped to the
// 700 token so `text-xs` clears WCAG AA on the 500/10 background.
export const MODE_META: Record<Mode, ModeMeta> = {
  plan: {
    label: "Plan",
    description: "Read and propose — for planning before making changes.",
    pillClass:
      "border-blue-500/40 bg-blue-500/10 text-blue-600 hover:bg-blue-500/20 dark:text-blue-400",
    dotClass: "bg-blue-500",
  },
  act: {
    label: "Act",
    description: "Full tools, with approval prompts for changes.",
    pillClass:
      "border-emerald-500/40 bg-emerald-500/10 text-emerald-700 hover:bg-emerald-500/20 dark:text-emerald-400",
    dotClass: "bg-emerald-500",
  },
  auto: {
    label: "Auto",
    description: "Proceed automatically; dangerous actions still prompt.",
    pillClass:
      "border-amber-500/40 bg-amber-500/10 text-amber-700 hover:bg-amber-500/20 dark:text-amber-400",
    dotClass: "bg-amber-500",
  },
};

/** Until modes gate the backend turn, the pill/settings surface this so the choice
 *  is never read as an active safety guarantee (review #287). */
export const MODE_NOT_ENFORCED_NOTE =
  "Display-only for now — modes don't gate the agent until backend wiring lands.";
