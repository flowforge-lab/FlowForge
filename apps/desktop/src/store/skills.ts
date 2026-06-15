// Installed skills + phenotypes cache for the ⌘K palette (#27 / #28). Refreshed
// on palette open and on `skills:changed` — the event carries the active set,
// but we re-fetch the full installed list for accurate `active` flags.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { Phenotype, SkillInfo } from "@/bindings";

interface SkillsState {
  skills: SkillInfo[];
  phenotypes: Phenotype[];
  activePhenotype: Phenotype | null;
  refresh: () => Promise<void>;
  search: (query: string) => Promise<SkillInfo[]>;
}

export const useSkillsStore = create<SkillsState>((set) => ({
  skills: [],
  phenotypes: [],
  activePhenotype: null,

  refresh: async () => {
    const [skills, phenotypes, activePhenotype] = await Promise.all([
      ipc.listSkills(),
      ipc.listPhenotypes(),
      ipc.getPhenotype(),
    ]);
    set({ skills, phenotypes, activePhenotype });
  },

  search: (query) => ipc.searchSkills(query),
}));
