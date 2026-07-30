import { useEffect } from "react";
import { SettingsSwitch } from "@/components/settings/switch";
import { useSettingsStore } from "@/store/settings";
import { useExperimentalStore, type FlagId } from "@/store/experimental";

interface FlagMeta {
  id: FlagId;
  label: string;
  description: string;
  /** Flags whose backend can only attach at launch show a "restart required" note. */
  restartRequired?: boolean;
}

// Order shown in the section. Each description documents the future backend the
// flag will gate (SET.10 ships the flags only; the behaviors are out of scope).
const FLAGS: readonly FlagMeta[] = [
  {
    id: "ownApiKey",
    label: "Use your own API key",
    description:
      "Route turns through a cloud provider key you supply instead of the local model server.",
  },
  {
    id: "spotlight",
    label: "Spotlight",
    description: "Summon FlowForge from anywhere with a global hotkey overlay.",
    restartRequired: true,
  },
  {
    id: "preventSleep",
    label: "Prevent sleep",
    description: "Keep the machine awake while a turn is streaming.",
  },
  {
    id: "remoteExecution",
    label: "Remote execution",
    description:
      "Run tools on a configured remote host rather than this machine.",
    restartRequired: true,
  },
  {
    id: "backgroundObservers",
    label: "Background observers",
    description:
      "Let signal observers watch sessions and surface intentions in the background.",
    restartRequired: true,
  },
  {
    id: "smartSkillSurfacing",
    label: "Smart skill surfacing",
    description:
      "Proactively suggest relevant skills based on the current conversation.",
  },
  {
    id: "stepTimelineExport",
    label: "Step timeline export",
    description:
      "Show a developer download on each step group to export its per-step timing (JSON/CSV) for diagnosing slow runs.",
  },
  {
    id: "localUpdateChannel",
    label: "Local update channel",
    description:
      "Enable the background update poll in a dev build so the global update bar picks up a local dev-release.sh feed (D1, local updater endpoint). Set FF_UPDATER_ENDPOINT when you turn this on: without it the poll falls through to the default public GitHub feed (inert only while your dev version matches the latest release). Not dev-install.sh (D2, direct file swap, no feed).",
    restartRequired: true,
  },
  {
    id: "devTools",
    label: "Developer tools",
    description:
      'Show the "Developer" group in the About section (the sidecar smoke-test button). Off by default so dev-only surfaces stay off user installs.',
  },
];

/**
 * Experimental section (#133, SET.10): a vertical list of opt-in feature flags.
 * All flags are FE-only and default off; the footer "Reset to defaults" turns
 * them all back off.
 */
export function ExperimentalSection() {
  const flags = useExperimentalStore((s) => s.flags);
  const setFlag = useExperimentalStore((s) => s.setFlag);
  const resetExperimental = useExperimentalStore((s) => s.resetExperimental);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  useEffect(() => {
    registerResetHandler(resetExperimental);
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetExperimental]);

  return (
    <div className="space-y-1">
      <p className="text-[12px] leading-relaxed text-muted-foreground">
        Opt-in features that are still in progress. They may change or be
        removed.
      </p>
      <div className="divide-y">
        {FLAGS.map((flag) => (
          <SettingsSwitch
            key={flag.id}
            className="py-4 first:pt-3"
            label={flag.label}
            description={
              flag.restartRequired
                ? `${flag.description} Restart required.`
                : flag.description
            }
            checked={flags[flag.id]}
            onCheckedChange={(on) => setFlag(flag.id, on)}
          />
        ))}
      </div>
    </div>
  );
}
