// Profiles (phenotypes) management surface for the Profiles section (#130, SET.7).
// A "Profile" is a FE presentation view over the backend `Phenotype` binding —
// it folds the phenotype's name/skills/persona into card fields and adds FE-only
// presentation (a stable accent, a locked flag for the built-in default). The
// active profile round-trips through the phenotype IPC (`switchPhenotype`), so it
// persists exactly like the ⌘K `pheno` switcher.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { Phenotype } from "@/bindings";

/** FE-only accent assigned per card (stable by list position). */
export type ProfileAccent = "blue" | "violet" | "emerald" | "amber" | "rose";

const ACCENTS: readonly ProfileAccent[] = [
  "blue",
  "violet",
  "emerald",
  "amber",
  "rose",
];

/** The built-in phenotype id — locked and shown with the default star. */
export const DEFAULT_PROFILE_ID = "default";

/** A profile card's view model. `id`/`name`/`skillCount` map from `Phenotype`;
 *  `locked`/`accent` are FE-only presentation. */
export interface Profile {
  id: string;
  name: string;
  description: string;
  skillCount: number;
  locked: boolean;
  accent: ProfileAccent;
}

/** Title-case a phenotype name for display (`data-science` → `Data Science`). */
function displayName(name: string): string {
  return name
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** Map a backend `Phenotype` to a `Profile` card. `index` (its position in the
 *  list) seeds a stable accent. The built-in `default` is locked. */
export function phenotypeToProfile(pheno: Phenotype, index: number): Profile {
  const description =
    pheno.persona ??
    (pheno.skills.length > 0
      ? pheno.skills.join(", ")
      : "No skills active — the base working set.");
  return {
    id: pheno.name,
    name: displayName(pheno.name),
    description,
    skillCount: pheno.skills.length,
    locked: pheno.name === DEFAULT_PROFILE_ID,
    accent: ACCENTS[index % ACCENTS.length],
  };
}

interface ProfilesState {
  profiles: Profile[];
  /** Id of the active profile (the active phenotype's name). */
  activeId: string;
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setActive: (id: string) => Promise<void>;
  resetProfiles: () => Promise<void>;
}

export const useProfilesStore = create<ProfilesState>((set, get) => ({
  profiles: [],
  activeId: DEFAULT_PROFILE_ID,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const [phenotypes, active] = await Promise.all([
        ipc.listPhenotypes(),
        ipc.getPhenotype(),
      ]);
      set({
        profiles: phenotypes.map(phenotypeToProfile),
        activeId: active.name,
        loading: false,
      });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setActive: async (id) => {
    const prev = get().activeId;
    if (id === prev) return;
    // Optimistic: highlight the card immediately, revert if the switch rejects.
    set({ activeId: id, saving: true, error: null });
    try {
      const active = await ipc.switchPhenotype(id);
      set({ activeId: active.name, saving: false });
    } catch (err) {
      set({
        activeId: prev,
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  // Footer reset: return to the built-in default profile.
  resetProfiles: () => get().setActive(DEFAULT_PROFILE_ID),
}));
