// Control settings for the Control section (#127). Loaded from / persisted via
// IPC — the backend (mock for now) owns durable storage; this store is the UI
// cache. Mirrors store/search-config.ts. The whole config round-trips, so a
// `setX` applies an optimistic update and persists the full config.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import {
  CONTROL_DEFAULTS,
  slugify,
  type ControlConfig,
  type ControlUi,
  type Teammate,
} from "@/lib/control";

interface ControlConfigState {
  config: ControlConfig | null;
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setInjectMemory: (inject: boolean) => Promise<void>;
  setUserInstructions: (text: string) => Promise<void>;
  addPromptFile: (path: string) => Promise<void>;
  removePromptFile: (path: string) => Promise<void>;
  /** Add a teammate (SET.12). Server assigns the id; no-op on a blank name.
   *  A blank slug auto-derives from the name (kebab-case). */
  addTeammate: (input: Omit<Teammate, "id">) => Promise<void>;
  /** Patch an existing teammate (#805). No-op if the id is unknown, the resulting
   *  name is blank, or the derived slug collides with a different teammate. */
  updateTeammate: (
    id: string,
    patch: Partial<Omit<Teammate, "id">>,
  ) => Promise<void>;
  removeTeammate: (id: string) => Promise<void>;
  /** Patch one or more UI-customization fields (SET.12). */
  setUi: (patch: Partial<ControlUi>) => Promise<void>;
  resetControl: () => Promise<void>;
}

export const useControlConfigStore = create<ControlConfigState>((set, get) => ({
  config: null,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const config = await ipc.getControlConfig();
      set({ config, loading: false });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setInjectMemory: (inject) =>
    persist(set, get, (c) => ({ ...c, injectMemory: inject })),

  setUserInstructions: (text) =>
    persist(set, get, (c) => ({ ...c, userInstructions: text })),

  addPromptFile: (path) => {
    const trimmed = path.trim();
    if (trimmed === "") return Promise.resolve();
    return persist(set, get, (c) =>
      c.promptFiles.includes(trimmed)
        ? c
        : { ...c, promptFiles: [...c.promptFiles, trimmed] },
    );
  },

  removePromptFile: (path) =>
    persist(set, get, (c) => ({
      ...c,
      promptFiles: c.promptFiles.filter((p) => p !== path),
    })),

  addTeammate: (input) => {
    const name = input.name.trim();
    if (name === "") return Promise.resolve();
    // Blank slug auto-derives from the name (#805); slugify normalizes either way.
    const slug = slugify(input.slug) || slugify(name);
    const teammate: Teammate = {
      id: crypto.randomUUID(),
      name,
      slug,
      description: input.description.trim(),
    };
    return persist(set, get, (c) =>
      // Dedupe on a non-empty handle (mirrors addPromptFile). A slug can still be
      // empty when the name has no alphanumerics; those stay optional and may repeat.
      slug !== "" && c.teammates.some((t) => t.slug === slug)
        ? c
        : { ...c, teammates: [...c.teammates, teammate] },
    );
  },

  updateTeammate: (id, patch) =>
    persist(set, get, (c) => {
      const existing = c.teammates.find((t) => t.id === id);
      if (!existing) return c; // unknown id
      const name = (patch.name ?? existing.name).trim();
      if (name === "") return c; // don't allow clearing the name
      const slug = slugify(patch.slug ?? existing.slug) || slugify(name);
      // Dedupe against the *other* teammates (a teammate never collides with itself).
      if (
        slug !== "" &&
        c.teammates.some((t) => t.id !== id && t.slug === slug)
      )
        return c;
      const description = (patch.description ?? existing.description).trim();
      return {
        ...c,
        teammates: c.teammates.map((t) =>
          t.id === id ? { ...t, name, slug, description } : t,
        ),
      };
    }),

  removeTeammate: (id) =>
    persist(set, get, (c) => ({
      ...c,
      teammates: c.teammates.filter((t) => t.id !== id),
    })),

  setUi: (patch) =>
    persist(set, get, (c) => ({ ...c, ui: { ...c.ui, ...patch } })),

  resetControl: () =>
    persist(set, get, () => structuredClone(CONTROL_DEFAULTS)),
}));

// Apply a pure transform to the current config and persist it via IPC, keeping
// the store optimistic and surfacing errors. No-op if the config hasn't loaded.
type SetFn = (partial: Partial<ControlConfigState>) => void;
type GetFn = () => ControlConfigState;

async function persist(
  set: SetFn,
  get: GetFn,
  update: (config: ControlConfig) => ControlConfig,
): Promise<void> {
  const { config } = get();
  if (!config) return;
  const next = update(config);
  if (next === config) return; // transform decided it was a no-op
  set({ config: next, saving: true, error: null });
  try {
    const stored = await ipc.setControlConfig(next);
    set({ config: stored, saving: false });
  } catch (err) {
    set({
      saving: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}
