import { describe, expect, it } from "vitest";
import {
  getSectionMeta,
  SETTINGS_NAV,
  type SettingsSectionId,
} from "@/components/settings/registry";

const ALL_IDS: SettingsSectionId[] = [
  "model",
  "skills",
  "control",
  "appearance",
  "profiles",
  "memory",
  "scheduled",
  "keyboard",
  "experimental",
  "about",
];

describe("settings registry", () => {
  it("groups sections as PROFILE then GLOBAL", () => {
    expect(SETTINGS_NAV.map((g) => g.group)).toEqual(["PROFILE", "GLOBAL"]);
  });

  it("lists all 10 section ids in canonical order across the two groups", () => {
    const flat = SETTINGS_NAV.flatMap((g) => g.items.map((i) => i.id));
    expect(flat).toEqual(ALL_IDS);
  });

  it("splits 3 PROFILE / 7 GLOBAL items", () => {
    const [profile, global] = SETTINGS_NAV;
    expect(profile.items.map((i) => i.id)).toEqual([
      "model",
      "skills",
      "control",
    ]);
    expect(global.items.map((i) => i.id)).toEqual([
      "appearance",
      "profiles",
      "memory",
      "scheduled",
      "keyboard",
      "experimental",
      "about",
    ]);
  });

  it('labels the GLOBAL keyboard item "Keyboard" (not "Shortcuts")', () => {
    expect(getSectionMeta("keyboard").label).toBe("Keyboard");
  });

  it("resolves a label + icon for every id", () => {
    for (const id of ALL_IDS) {
      const meta = getSectionMeta(id);
      expect(meta.id).toBe(id);
      expect(meta.label).toBeTruthy();
      expect(meta.icon).toBeTruthy();
    }
  });
});
