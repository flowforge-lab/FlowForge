import { beforeEach, describe, expect, it } from "vitest";

import { ipc } from "@/lib/ipc";
import {
  DEFAULT_PROFILE_ID,
  phenotypeToProfile,
  useProfilesStore,
} from "@/store/profiles";

describe("phenotypeToProfile", () => {
  it("maps phenotype fields and marks the built-in default as locked", () => {
    const p = phenotypeToProfile({ name: "default", skills: [] }, 0);
    expect(p).toMatchObject({
      id: "default",
      name: "Default",
      skillCount: 0,
      locked: true,
      accent: "blue",
    });
    expect(p.description).toMatch(/base working set/i);
  });

  it("title-cases names, counts skills, and uses persona as the description", () => {
    const p = phenotypeToProfile(
      { name: "data-science", skills: ["a", "b"], persona: "You crunch data." },
      1,
    );
    expect(p.name).toBe("Data Science");
    expect(p.skillCount).toBe(2);
    expect(p.locked).toBe(false);
    expect(p.description).toBe("You crunch data.");
  });

  it("assigns a stable accent by list position", () => {
    const accents = [0, 1, 2, 3, 4, 5].map(
      (i) => phenotypeToProfile({ name: `p${i}`, skills: [] }, i).accent,
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
      activeId: DEFAULT_PROFILE_ID,
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

  it("resetProfiles returns to the default profile", async () => {
    await useProfilesStore.getState().load();
    const other = useProfilesStore
      .getState()
      .profiles.find((p) => p.id !== DEFAULT_PROFILE_ID)!;
    await useProfilesStore.getState().setActive(other.id);

    await useProfilesStore.getState().resetProfiles();
    expect(useProfilesStore.getState().activeId).toBe(DEFAULT_PROFILE_ID);
  });
});
