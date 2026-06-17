import { useEffect, useState } from "react";
import { SubTabs } from "@/components/settings/sub-tabs";
import { InstalledTab } from "@/components/settings/skills/installed-tab";
import { MarketplaceTab } from "@/components/settings/skills/marketplace-tab";
import { ShortcutsTab } from "@/components/settings/skills/shortcuts-tab";
import { useSettingsStore } from "@/store/settings";
import { useCommandShortcutsStore } from "@/store/command-shortcuts";

type SkillsTab = "installed" | "marketplace" | "shortcuts";

const TABS: ReadonlyArray<{ value: SkillsTab; label: string }> = [
  { value: "installed", label: "Installed" },
  { value: "marketplace", label: "Marketplace" },
  { value: "shortcuts", label: "Shortcuts" },
];

/**
 * Skills section (#128, SET.5): Installed / Marketplace / Shortcuts sub-tabs. MCP
 * server config + live status live in the standalone GLOBAL "MCP servers" section
 * (#143), not here. The footer "Reset to defaults" clears `/name` shortcuts.
 */
export function SkillsSection() {
  const [tab, setTab] = useState<SkillsTab>("installed");
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);
  const resetShortcuts = useCommandShortcutsStore((s) => s.resetShortcuts);

  useEffect(() => {
    registerResetHandler(resetShortcuts);
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetShortcuts]);

  return (
    <SubTabs
      label="Skills sections"
      tabs={TABS}
      value={tab}
      onValueChange={setTab}
    >
      {tab === "installed" && <InstalledTab />}
      {tab === "marketplace" && <MarketplaceTab />}
      {tab === "shortcuts" && <ShortcutsTab />}
    </SubTabs>
  );
}
