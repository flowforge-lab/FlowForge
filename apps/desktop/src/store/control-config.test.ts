import { beforeEach, describe, expect, it } from "vitest";

import { useControlConfigStore } from "@/store/control-config";
import { policyForMode } from "@/lib/control";

// The store talks to the shared MockIpc singleton; load + reset to a clean
// baseline before each test.
beforeEach(async () => {
  await useControlConfigStore.getState().load();
  await useControlConfigStore.getState().resetControl();
});

const cfg = () => useControlConfigStore.getState().config!;

describe("control-config store", () => {
  it("loads the config from IPC", () => {
    expect(cfg().defaultMode).toBe("auto");
  });

  it("setDefaultMode updates the mode and re-derives the policy", async () => {
    await useControlConfigStore.getState().setDefaultMode("act");
    expect(cfg().defaultMode).toBe("act");
    expect(cfg().permissionPolicy).toEqual(policyForMode("act"));
  });

  it("adds and removes overrides, ignoring blanks and dupes", async () => {
    await useControlConfigStore.getState().addOverride("denied", "rm -rf");
    await useControlConfigStore.getState().addOverride("denied", "rm -rf"); // dupe
    await useControlConfigStore.getState().addOverride("denied", "  "); // blank
    expect(cfg().overrides.denied).toEqual(["rm -rf"]);

    await useControlConfigStore.getState().removeOverride("denied", "rm -rf");
    expect(cfg().overrides.denied).toEqual([]);
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
    await useControlConfigStore.getState().setDefaultMode("plan");
    await useControlConfigStore.getState().load();
    expect(cfg().defaultMode).toBe("plan");
  });

  it("resetControl restores defaults", async () => {
    await useControlConfigStore.getState().setDefaultMode("act");
    await useControlConfigStore.getState().addOverride("allowed", "ls");
    await useControlConfigStore.getState().resetControl();
    expect(cfg().defaultMode).toBe("auto");
    expect(cfg().overrides.allowed).toEqual([]);
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

    // An empty slug stays optional and may repeat.
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "No Handle", slug: "  ", description: "" });
    await useControlConfigStore
      .getState()
      .addTeammate({ name: "Also None", slug: "", description: "" });
    expect(cfg().teammates).toHaveLength(seeded + 3);
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
