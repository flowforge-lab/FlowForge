import { beforeEach, describe, expect, it } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  CODON_PROFILE_ID,
  DEFAULT_PROFILE_ID,
  defaultProfileId,
  phenotypeToProfile,
  useProfilesStore,
} from "@/store/profiles";

describe("defaultProfileId", () => {
  it("targets codon when it is installed", () => {
    const profiles = [
      phenotypeToProfile(
        { name: "default", skills: [], mcpServers: [], egress: "open" },
        0,
      ),
      phenotypeToProfile(
        {
          name: "codon",
          skills: ["codegraph"],
          mcpServers: [],
          egress: "open",
        },
        1,
      ),
    ];
    expect(defaultProfileId(profiles)).toBe(CODON_PROFILE_ID);
  });

  it("falls back to the built-in default when codon is absent", () => {
    const profiles = [
      phenotypeToProfile(
        { name: "default", skills: [], mcpServers: [], egress: "open" },
        0,
      ),
      phenotypeToProfile(
        { name: "rust", skills: [], mcpServers: [], egress: "open" },
        1,
      ),
    ];
    expect(defaultProfileId(profiles)).toBe(DEFAULT_PROFILE_ID);
  });
});

describe("phenotypeToProfile", () => {
  it("maps phenotype fields and marks the built-in default as locked", () => {
    const p = phenotypeToProfile(
      { name: "default", skills: [], mcpServers: [], egress: "open" },
      0,
    );
    expect(p).toMatchObject({
      id: "default",
      name: "Default",
      skillCount: 0,
      locked: true,
      accent: "blue",
    });
    expect(p.description).toMatch(/base working set/i);
  });

  it("leaves codon unlocked — it is user-installed content, not the built-in", () => {
    const p = phenotypeToProfile(
      { name: "codon", skills: ["codegraph"], mcpServers: [], egress: "open" },
      1,
    );
    expect(p.id).toBe("codon");
    expect(p.locked).toBe(false);
  });

  it("title-cases names, counts skills, and uses persona as the description", () => {
    const p = phenotypeToProfile(
      {
        name: "data-science",
        skills: ["a", "b"],
        persona: "You crunch data.",
        mcpServers: [],
        egress: "open",
      },
      1,
    );
    expect(p.name).toBe("Data Science");
    expect(p.skillCount).toBe(2);
    expect(p.locked).toBe(false);
    expect(p.description).toBe("You crunch data.");
  });

  it("assigns a stable accent by list position", () => {
    const accents = [0, 1, 2, 3, 4, 5].map(
      (i) =>
        phenotypeToProfile(
          { name: `p${i}`, skills: [], mcpServers: [], egress: "open" },
          i,
        ).accent,
    );
    expect(accents).toEqual([
      "blue",
      "violet",
      "emerald",
      "amber",
      "rose",
      "blue", // wraps
    ]);
  });
});

describe("useProfilesStore", () => {
  beforeEach(async () => {
    await ipc.switchPhenotype(DEFAULT_PROFILE_ID);
    useProfilesStore.setState({
      profiles: [],
      phenotypesById: {},
      activeId: DEFAULT_PROFILE_ID,
      selectedId: null,
      loading: false,
      saving: false,
      error: null,
    });
  });

  it("loads profiles and the active id from the phenotype IPC", async () => {
    await useProfilesStore.getState().load();
    const { profiles, activeId } = useProfilesStore.getState();
    expect(profiles.length).toBeGreaterThan(1);
    expect(profiles[0].id).toBe(DEFAULT_PROFILE_ID);
    expect(activeId).toBe(DEFAULT_PROFILE_ID);
  });

  it("setActive switches the active phenotype and persists it", async () => {
    await useProfilesStore.getState().load();
    const other = useProfilesStore
      .getState()
      .profiles.find((p) => p.id !== DEFAULT_PROFILE_ID)!;

    await useProfilesStore.getState().setActive(other.id);
    expect(useProfilesStore.getState().activeId).toBe(other.id);
    // Persisted server-side: a fresh read reflects the switch.
    expect((await ipc.getPhenotype()).name).toBe(other.id);
  });

  it("reverts the optimistic active id when the switch rejects", async () => {
    await useProfilesStore.getState().setActive("does-not-exist");
    expect(useProfilesStore.getState().activeId).toBe(DEFAULT_PROFILE_ID);
    expect(useProfilesStore.getState().error).toMatch(/unknown phenotype/i);
  });

  it("resetProfiles returns to the out-of-box default (codon when installed)", async () => {
    await useProfilesStore.getState().load();
    // Switch away from the out-of-box default to something else, then reset.
    const other = useProfilesStore
      .getState()
      .profiles.find(
        (p) => p.id !== DEFAULT_PROFILE_ID && p.id !== CODON_PROFILE_ID,
      )!;
    await useProfilesStore.getState().setActive(other.id);

    await useProfilesStore.getState().resetProfiles();
    // The mock seeds codon, so reset targets codon rather than the built-in default.
    expect(useProfilesStore.getState().activeId).toBe(CODON_PROFILE_ID);
  });
});

describe("useProfilesStore editor (#530)", () => {
  beforeEach(async () => {
    useProfilesStore.setState({
      profiles: [],
      phenotypesById: {},
      activeId: DEFAULT_PROFILE_ID,
      selectedId: null,
      loading: false,
      saving: false,
      error: null,
    });
    await useProfilesStore.getState().load();
  });

  it("load keeps the raw phenotype and auto-selects the active one", () => {
    const { phenotypesById, selectedId, activeId } =
      useProfilesStore.getState();
    expect(phenotypesById[DEFAULT_PROFILE_ID]).toMatchObject({
      name: "default",
    });
    // First open targets the active phenotype so the editor is discoverable.
    expect(selectedId).toBe(activeId);
  });

  it("select opens / closes a phenotype's editor panel", () => {
    useProfilesStore.getState().select("rust");
    expect(useProfilesStore.getState().selectedId).toBe("rust");
    useProfilesStore.getState().select(null);
    expect(useProfilesStore.getState().selectedId).toBeNull();
  });

  it("savePhenotype binds provider/model losslessly and round-trips through load", async () => {
    await useProfilesStore
      .getState()
      .savePhenotype("rust", { provider: "openai", model: "gpt-4o" });
    expect(useProfilesStore.getState().phenotypesById["rust"]).toMatchObject({
      provider: "openai",
      model: "gpt-4o",
      // Skills/persona are preserved (whole-record write).
      skills: ["rust-debugging", "write-tests"],
      persona: "You are a meticulous Rust engineer.",
    });
    // Persisted in the backend: a fresh read still has the binding.
    const raw = (await ipc.listPhenotypes()).find((p) => p.name === "rust");
    expect(raw).toMatchObject({ provider: "openai", model: "gpt-4o" });
  });

  it("savePhenotype with undefined clears the binding (inherit)", async () => {
    await useProfilesStore
      .getState()
      .savePhenotype("reviewer", { provider: undefined, model: undefined });
    const reviewer = useProfilesStore.getState().phenotypesById["reviewer"];
    expect(reviewer.provider).toBeUndefined();
    expect(reviewer.model).toBeUndefined();
  });

  it("savePhenotype records the error when the write rejects", async () => {
    await useProfilesStore
      .getState()
      .savePhenotype("rust", { provider: "ghost-conn" });
    expect(useProfilesStore.getState().error).toMatch(/unknown connection/i);
  });

  it("duplicatePhenotype creates a uniquely-named clone and selects it", async () => {
    await useProfilesStore.getState().duplicatePhenotype(DEFAULT_PROFILE_ID);
    const { profiles, selectedId } = useProfilesStore.getState();
    expect(profiles.map((p) => p.id)).toContain("default-copy");
    expect(selectedId).toBe("default-copy");

    // A second duplicate avoids the name collision.
    await useProfilesStore.getState().duplicatePhenotype(DEFAULT_PROFILE_ID);
    expect(useProfilesStore.getState().selectedId).toBe("default-copy-2");
  });
});
