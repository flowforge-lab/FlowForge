// Single source of truth for the Settings information architecture (SET.1a).
// The section-id union, nav grouping, labels and icons all live here — consumers
// (the shell, the nav, the store) import from this module and never re-declare
// the union ad hoc.

import {
  Blocks,
  Brain,
  CalendarClock,
  Cpu,
  FlaskConical,
  Info,
  Keyboard,
  Palette,
  SlidersHorizontal,
  Users,
  type LucideIcon,
} from "lucide-react";

export type SettingsSectionId =
  | "model" // PROFILE
  | "skills"
  | "control"
  | "appearance" // GLOBAL
  | "profiles"
  | "memory"
  | "scheduled"
  | "keyboard"
  | "experimental"
  | "about";

/** Nav group headers (rendered uppercase). */
export type SettingsGroup = "PROFILE" | "GLOBAL";

export interface SettingsSectionMeta {
  id: SettingsSectionId;
  label: string;
  icon: LucideIcon;
}

export interface SettingsNavGroup {
  group: SettingsGroup;
  items: SettingsSectionMeta[];
}

// The nav is the canonical ordering. Note GLOBAL uses "keyboard"/"Keyboard" —
// distinct from the Skills section's "Shortcuts" sub-tab (a different concept).
export const SETTINGS_NAV: readonly SettingsNavGroup[] = [
  {
    group: "PROFILE",
    items: [
      { id: "model", label: "Model", icon: Cpu },
      { id: "skills", label: "Skills", icon: Blocks },
      { id: "control", label: "Control", icon: SlidersHorizontal },
    ],
  },
  {
    group: "GLOBAL",
    items: [
      { id: "appearance", label: "Appearance", icon: Palette },
      { id: "profiles", label: "Profiles", icon: Users },
      { id: "memory", label: "Memory", icon: Brain },
      { id: "scheduled", label: "Scheduled", icon: CalendarClock },
      { id: "keyboard", label: "Keyboard", icon: Keyboard },
      { id: "experimental", label: "Experimental", icon: FlaskConical },
      { id: "about", label: "About", icon: Info },
    ],
  },
] as const;

/** Flat id → meta lookup, derived from the nav so the two never drift. */
const SECTION_META: Record<SettingsSectionId, SettingsSectionMeta> =
  Object.fromEntries(
    SETTINGS_NAV.flatMap((g) => g.items).map((item) => [item.id, item]),
  ) as Record<SettingsSectionId, SettingsSectionMeta>;

export function getSectionMeta(id: SettingsSectionId): SettingsSectionMeta {
  return SECTION_META[id];
}
