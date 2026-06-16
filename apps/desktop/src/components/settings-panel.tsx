import { SettingsShell } from "@/components/settings/settings-shell";
import { useSettingsStore } from "@/store/settings";

/** Mounts the centered settings modal when open (SET.1a). */
export function SettingsPanel() {
  const open = useSettingsStore((s) => s.open);
  if (!open) return null;
  return <SettingsShell />;
}
