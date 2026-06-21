// Presentation metadata for the agent modes (#266, RFC 0011). Kept React-free so
// it's unit-testable and shared by the composer pill and the Settings control.
// Colour coding: Plan = blue/calm, Act = green/go, Auto = amber/caution.

import type { Mode } from "@/bindings";

// Direct mode hotkeys (#267): ⌘P → Plan, ⌘T → Act, ⌘O → Auto. Kept here (React-free)
// so the mapping is unit-testable and shared with the app-shell handler + registry.
const MODE_HOTKEYS: Record<string, Mode> = { p: "plan", t: "act", o: "auto" };

/** The mode a bare key (without the modifier) selects, or undefined for other keys. */
export function modeForHotkey(key: string): Mode | undefined {
  return MODE_HOTKEYS[key.toLowerCase()];
}

export interface ModeMeta {
  label: string;
  /** One-liner shown on hover / under the Settings control. */
  description: string;
  /** Tailwind classes for the pill (border + tint + text), light & dark. */
  pillClass: string;
  /** Tailwind classes for the status dot. */
  dotClass: string;
}

// Descriptions state what each mode does to the turn (#265 wired the backend gate):
// Plan advertises only ReadOnly tools, Act prompts on every Write, Auto auto-approves
// Write. Light tints bumped to the 700 token so `text-xs` clears WCAG AA on the
// 500/10 background.
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
