import { useEffect, useState } from "react";
import { SubTabs } from "@/components/settings/sub-tabs";
import { PermissionsTab } from "@/components/settings/control/permissions-tab";
import { PromptsTab } from "@/components/settings/control/prompts-tab";
import { TeamTab } from "@/components/settings/control/team-tab";
import { UiTab } from "@/components/settings/control/ui-tab";
import { useSettingsStore } from "@/store/settings";
import { useControlConfigStore } from "@/store/control-config";

type ControlTab = "permissions" | "prompts" | "team" | "ui";

const TABS: ReadonlyArray<{ value: ControlTab; label: string }> = [
  { value: "permissions", label: "Permissions" },
  { value: "prompts", label: "Prompts" },
  { value: "team", label: "Team" },
  { value: "ui", label: "UI" },
];

/**
 * Control section (#127, +SET.12): Permissions / Prompts / Team / UI sub-tabs.
 * Loads the control config on mount and wires the footer "Reset to defaults" to
 * `resetControl`.
 */
export function ControlSection() {
  const [tab, setTab] = useState<ControlTab>("permissions");
  const load = useControlConfigStore((s) => s.load);
  const resetControl = useControlConfigStore((s) => s.resetControl);
  const error = useControlConfigStore((s) => s.error);
  const config = useControlConfigStore((s) => s.config);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    registerResetHandler(() => void resetControl());
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetControl]);

  return (
    <div className="space-y-4">
      {error ? (
        <p className="text-[12px] text-destructive" role="alert">
          {error}
        </p>
      ) : null}
      {config === null ? (
        <p className="text-[12px] text-muted-foreground">Loading…</p>
      ) : (
        <SubTabs
          label="Control sections"
          tabs={TABS}
          value={tab}
          onValueChange={setTab}
        >
          {tab === "permissions" && <PermissionsTab />}
          {tab === "prompts" && <PromptsTab />}
          {tab === "team" && <TeamTab />}
          {tab === "ui" && <UiTab />}
        </SubTabs>
      )}
    </div>
  );
}
