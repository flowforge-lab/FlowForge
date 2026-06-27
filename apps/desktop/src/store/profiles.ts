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

/** The built-in phenotype id — always present and locked (cannot be removed). */
export const DEFAULT_PROFILE_ID = "default";

/** The out-of-box default phenotype id (#298). Seeded on first run (#304); when
 *  present it is the starred "Default Phenotype", but unlike the built-in it is
 *  user-installed content, so it is NOT locked. */
export const CODON_PROFILE_ID = "codon";

/** The id of the out-of-box default profile for the current install: `codon` when
 *  it's installed, else the built-in `default`. Drives the ⭐ star and the footer
 *  "Reset to defaults". */
export function defaultProfileId(profiles: Profile[]): string {
  return profiles.some((p) => p.id === CODON_PROFILE_ID)
    ? CODON_PROFILE_ID
    : DEFAULT_PROFILE_ID;
}

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

/** Append a `-copy` suffix to `base`, bumping `-copy-2`, `-copy-3`… until the name is
 *  free among `taken` (phenotype names). Used by "Duplicate to customize" (#530). */
function uniqueCopyName(base: string, taken: Set<string>): string {
  const stem = `${base}-copy`;
  if (!taken.has(stem)) return stem;
  for (let n = 2; ; n++) {
    const candidate = `${stem}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}

interface ProfilesState {
  profiles: Profile[];
  /** Raw `Phenotype` records keyed by name, kept alongside the lossy `Profile` view
   *  so the editor write path is a lossless read-modify-write (#530). */
  phenotypesById: Record<string, Phenotype>;
  /** Id of the active profile (the active phenotype's name). */
  activeId: string;
  /** The phenotype whose detail/editor panel is open, or null when none (#530). */
  selectedId: string | null;
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setActive: (id: string) => Promise<void>;
  select: (id: string | null) => void;
  savePhenotype: (
    id: string,
    patch: Partial<Pick<Phenotype, "provider" | "model">>,
  ) => Promise<void>;
  duplicatePhenotype: (sourceId: string) => Promise<void>;
  resetProfiles: () => Promise<void>;
}

export const useProfilesStore = create<ProfilesState>((set, get) => ({
  profiles: [],
  phenotypesById: {},
  activeId: DEFAULT_PROFILE_ID,
  selectedId: null,
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
      const phenotypesById = Object.fromEntries(
        phenotypes.map((p) => [p.name, p]),
      );
      set((s) => ({
        profiles: phenotypes.map(phenotypeToProfile),
        phenotypesById,
        activeId: active.name,
        // Keep a still-valid selection across reloads; otherwise default to the
        // active phenotype so the editor is discoverable on first open.
        selectedId:
          s.selectedId && phenotypesById[s.selectedId]
            ? s.selectedId
            : active.name,
        loading: false,
      }));
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

  // Open (or close, with `null`) a phenotype's detail/editor panel (#530).
  select: (id) => set({ selectedId: id }),

  // Persist edited provider/model on a phenotype (#530). Lossless: spread the full
  // raw `Phenotype` and overlay the patch, so skills/persona/maxIterations are
  // preserved through the round-trip. The built-in `default` is immutable and never
  // reaches this path (the editor renders it read-only). `provider`/`model` set to
  // undefined clears the binding (inherit the global tier).
  savePhenotype: async (id, patch) => {
    const base = get().phenotypesById[id];
    if (!base) return;
    set({ saving: true, error: null });
    try {
      const saved = await ipc.updatePhenotype({ ...base, ...patch });
      set((s) => ({
        phenotypesById: { ...s.phenotypesById, [saved.name]: saved },
        profiles: s.profiles.map((p) =>
          p.id === saved.name
            ? phenotypeToProfile(saved, s.profiles.indexOf(p))
            : p,
        ),
        saving: false,
      }));
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  // "Duplicate to customize" (#530): clone a phenotype (e.g. the immutable `default`)
  // into a new, editable one under a free `<source>-copy` name, then select it so the
  // editor opens on the copy. Reloads to pick up the new record from the backend.
  duplicatePhenotype: async (sourceId) => {
    const source = get().phenotypesById[sourceId];
    if (!source) return;
    const name = uniqueCopyName(
      sourceId,
      new Set(Object.keys(get().phenotypesById)),
    );
    set({ saving: true, error: null });
    try {
      await ipc.updatePhenotype({ ...source, name });
      await get().load();
      set({ selectedId: name, saving: false });
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  // Footer reset: return to the out-of-box default (codon when installed, else the
  // built-in default).
  resetProfiles: () => get().setActive(defaultProfileId(get().profiles)),
}));
