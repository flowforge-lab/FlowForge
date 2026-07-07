import { beforeEach, describe, expect, it } from "vitest";

import { useControlConfigStore } from "@/store/control-config";

// The store talks to the shared MockIpc singleton; load + reset to a clean
// baseline before each test.
beforeEach(async () => {
  await useControlConfigStore.getState().load();
  await useControlConfigStore.getState().resetControl();
});

const cfg = () => useControlConfigStore.getState().config!;

describe("control-config store", () => {
  it("loads the config from IPC", () => {
    // Default mode moved to the backend `mode.json` / prefs (#798); the control
    // config no longer carries it.
    expect(cfg().injectMemory).toBe(true);
    expect("defaultMode" in cfg()).toBe(false);
  });

  it("manages prompt files and toggles injectMemory + userInstructions", async () => {
    await useControlConfigStore
      .getState()
      .addPromptFile("{workspace}/AGENTS.md");
    await useControlConfigStore
      .getState()
      .addPromptFile("{workspace}/AGENTS.md"); // dupe
    expect(cfg().promptFiles).toEqual(["{workspace}/AGENTS.md"]);

    await useControlConfigStore.getState().setInjectMemory(false);
    await useControlConfigStore.getState().setUserInstructions("Be terse.");
    expect(cfg().injectMemory).toBe(false);
    expect(cfg().userInstructions).toBe("Be terse.");

    await useControlConfigStore
      .getState()
      .removePromptFile("{workspace}/AGENTS.md");
    expect(cfg().promptFiles).toEqual([]);
  });

  it("persists through IPC (a fresh load echoes the change)", async () => {
    await useControlConfigStore.getState().setInjectMemory(false);
    await useControlConfigStore.getState().load();
    expect(cfg().injectMemory).toBe(false);
  });

  it("resetControl restores defaults", async () => {
    await useControlConfigStore.getState().setInjectMemory(false);
    await useControlConfigStore.getState().resetControl();
    expect(cfg().injectMemory).toBe(true);
  });

  // ── SET.12: Team + UI ──────────────────────────────────────────────────────

  it("seeds default teammates and adds/removes (ignoring blank names)", async () => {
    const seeded = cfg().teammates.length;
    expect(seeded).toBeGreaterThan(0);

    await useControlConfigStore
      .getState()
      .addTeammate({ name: "  ", slug: "x", description: "" }); // blank → no-op
    expect(cfg().teammates).toHaveLength(seeded);

    await useControlConfigStore.getState().addTeammate({
      name: "Quinn QA",
      slug: "qa",
      description: "Runs tests.",
    });
    const added = cfg().teammates[cfg().teammates.length - 1];
    expect(added.name).toBe("Quinn QA");
    expect(added.id).toBeTruthy();

    await useControlConfigStore.getState().removeTeammate(added.id);
    expect(cfg().teammates).toHaveLength(seeded);
  });

  it("slugifies the handle and dedupes on a non-empty slug", async () => {
    const seeded = cfg().teammates.length;

    await useControlConfigStore.getState().addTeammate({
      name: "Quinn QA",
      slug: "Quinn QA!",
      description: "",
    });
    expect(cfg().teammates[cfg().teammates.length - 1].slug).toBe("quinn-qa");

    // Same handle (case/space-insensitive) → no-op; a different name doesn't matter.
    await useControlConfigStore.getState().addTeammate({
      name: "Quentin",
      slug: "quinn-qa",
      description: "",
    });
    expect(cfg().teammates).toHaveLength(seeded + 1);
  });

  it("derives a kebab-case slug from the name when the slug is blank (#805)", async () => {
    const seeded = cfg().teammates.length;

    await useControlConfigStore
      .getState()
      .addTeammate({ name: "No Handle", slug: "  ", description: "" });
    expect(cfg().teammates[cfg().teammates.length - 1].slug).toBe("no-handle");

    await useControlConfigStore
      .getState()
      .addTeammate({ name: "Also None", slug: "", description: "" });
    expect(cfg().teammates[cfg().teammates.length - 1].slug).toBe("also-none");
    expect(cfg().teammates).toHaveLength(seeded + 2);

    // A blank slug that derives to an existing handle now dedupes (was appended before).
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "No Handle", slug: "", description: "dupe" });
    expect(cfg().teammates).toHaveLength(seeded + 2);
  });

  it("updateTeammate edits an existing teammate and persists (#805)", async () => {
    await useControlConfigStore.getState().addTeammate({
      name: "Quinn QA",
      slug: "qa",
      description: "Runs tests.",
    });
    const id = cfg().teammates[cfg().teammates.length - 1].id;

    await useControlConfigStore
      .getState()
      .updateTeammate(id, { name: "Quinn Quality", description: "Owns QA." });
    const edited = cfg().teammates.find((t) => t.id === id)!;
    expect(edited.name).toBe("Quinn Quality");
    expect(edited.description).toBe("Owns QA.");
    expect(edited.slug).toBe("qa"); // untouched fields keep their value

    // A fresh load echoes the change (round-trips through IPC).
    await useControlConfigStore.getState().load();
    expect(cfg().teammates.find((t) => t.id === id)?.name).toBe(
      "Quinn Quality",
    );
  });

  it("updateTeammate derives a blank slug from the (possibly new) name (#805)", async () => {
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "Quinn QA", slug: "qa", description: "" });
    const id = cfg().teammates[cfg().teammates.length - 1].id;

    await useControlConfigStore
      .getState()
      .updateTeammate(id, { name: "Quinn Quality", slug: "" });
    expect(cfg().teammates.find((t) => t.id === id)?.slug).toBe(
      "quinn-quality",
    );
  });

  it("updateTeammate no-ops on unknown id, empty name, or a colliding slug (#805)", async () => {
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "Quinn QA", slug: "qa", description: "" });
    await useControlConfigStore.getState().addTeammate({
      name: "Riley Reviewer Two",
      slug: "reviewer2",
      description: "",
    });
    const teammates = cfg().teammates;
    const first = teammates[teammates.length - 2];
    const before = JSON.stringify(cfg().teammates);

    // Unknown id → no-op.
    await useControlConfigStore
      .getState()
      .updateTeammate("nope", { name: "X" });
    // Empty name → no-op.
    await useControlConfigStore
      .getState()
      .updateTeammate(first.id, { name: "   " });
    // Slug colliding with a *different* teammate → no-op.
    await useControlConfigStore
      .getState()
      .updateTeammate(first.id, { slug: "reviewer2" });
    expect(JSON.stringify(cfg().teammates)).toBe(before);

    // Keeping its own slug is not a self-collision.
    await useControlConfigStore
      .getState()
      .updateTeammate(first.id, { name: "Quinn QA Edited" });
    expect(cfg().teammates.find((t) => t.id === first.id)?.name).toBe(
      "Quinn QA Edited",
    );
  });

  it("patches UI fields without clobbering the others", async () => {
    await useControlConfigStore.getState().setUi({ accentColor: "#10b981" });
    await useControlConfigStore.getState().setUi({ logoPath: "/tmp/logo.png" });
    expect(cfg().ui.accentColor).toBe("#10b981");
    expect(cfg().ui.logoPath).toBe("/tmp/logo.png");
    expect(cfg().ui.contextualGreeting).toBe(true); // untouched default
  });

  it("resetControl also resets Team + UI", async () => {
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "Temp", slug: "temp", description: "" });
    await useControlConfigStore
      .getState()
      .setUi({ accentColor: "#ef4444", contextualGreeting: false });

    await useControlConfigStore.getState().resetControl();
    expect(cfg().ui.accentColor).toBe("#6366f1");
    expect(cfg().ui.contextualGreeting).toBe(true);
    expect(cfg().teammates.some((t) => t.name === "Temp")).toBe(false);
  });
});
