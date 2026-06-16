import { useEffect, useState } from "react";
import { SubTabs } from "@/components/settings/sub-tabs";
import { ThemeTab } from "@/components/settings/appearance/theme-tab";
import { NotificationsTab } from "@/components/settings/appearance/notifications-tab";
import { AdvancedTab } from "@/components/settings/appearance/advanced-tab";
import { useSettingsStore } from "@/store/settings";
import { usePrefsStore } from "@/store/prefs";

type AppearanceTab = "theme" | "notifications" | "advanced";

const TABS: ReadonlyArray<{ value: AppearanceTab; label: string }> = [
  { value: "theme", label: "Theme" },
  { value: "notifications", label: "Notifications" },
  { value: "advanced", label: "Advanced" },
];

/**
 * Appearance section — Theme / Notifications / Advanced sub-tabs (SET.2). Wires
 * the footer "Reset to defaults" to `resetAppearance` while this section is active.
 */
export function AppearanceSection() {
  const [tab, setTab] = useState<AppearanceTab>("theme");
  const registerResetHandler = useSettingsStore((s) => s.registerResetHandler);
  const resetAppearance = usePrefsStore((s) => s.resetAppearance);

  useEffect(() => {
    registerResetHandler(resetAppearance);
    return () => registerResetHandler(null);
  }, [registerResetHandler, resetAppearance]);

  return (
    // Pass the active pane as `children` so SubTabs renders it inside
    // `Tabs.Content` and keeps the tab↔panel a11y wiring intact.
    <SubTabs
      label="Appearance sections"
      tabs={TABS}
      value={tab}
      onValueChange={setTab}
    >
      {tab === "theme" && <ThemeTab />}
      {tab === "notifications" && <NotificationsTab />}
      {tab === "advanced" && <AdvancedTab />}
    </SubTabs>
  );
}
