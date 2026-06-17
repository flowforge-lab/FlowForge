import { useEffect, useState } from "react";
import { SubTabs } from "@/components/settings/sub-tabs";
import { InstalledTab } from "@/components/settings/profiles/installed-tab";
import { MarketplaceTab } from "@/components/settings/profiles/marketplace-tab";
import { useSettingsStore } from "@/store/settings";
import { useProfilesStore } from "@/store/profiles";

type ProfilesTab = "installed" | "marketplace";

const TABS: ReadonlyArray<{ value: ProfilesTab; label: string }> = [
  { value: "installed", label: "Installed" },
  { value: "marketplace", label: "Marketplace" },
];

/**
 * Profiles section (#130, SET.7): Installed / Marketplace sub-tabs over phenotypes.
 * Loads profiles on mount and wires the footer "Reset to defaults" to return to the
 * built-in default profile.
 */
export function ProfilesSection() {
  const [tab, setTab] = useState<ProfilesTab>("installed");
  const load = useProfilesStore((s) => s.load);
  const resetProfiles = useProfilesStore((s) => s.resetProfiles);
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    registerResetHandler(() => void resetProfiles());
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetProfiles]);

  return (
    <SubTabs
      label="Profiles sections"
      tabs={TABS}
      value={tab}
      onValueChange={setTab}
    >
      {tab === "installed" && <InstalledTab />}
      {tab === "marketplace" && <MarketplaceTab />}
    </SubTabs>
  );
}
