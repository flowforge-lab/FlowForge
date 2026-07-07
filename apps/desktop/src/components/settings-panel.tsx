import { lazy, Suspense } from "react";
import { useSettingsStore } from "@/store/settings";

// Lazy-load the settings shell (and, through it, every section) so none of the
// settings UI is in the initial bundle (#599 item 6). The `open` gate lives here,
// BEFORE the lazy element is rendered, so the chunk is fetched only when the user
// first opens Settings — not at first paint. `SettingsPanel` itself stays a tiny
// static import (just this open-check).
const SettingsShell = lazy(() =>
  import("@/components/settings/settings-shell").then((m) => ({
    default: m.SettingsShell,
  })),
);

/** Mounts the centered settings modal when open (SET.1a). */
export function SettingsPanel() {
  const open = useSettingsStore((s) => s.open);
  if (!open) return null;
  return (
    <Suspense fallback={null}>
      <SettingsShell />
    </Suspense>
  );
}
