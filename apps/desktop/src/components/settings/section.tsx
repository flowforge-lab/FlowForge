import { AppearanceSection } from "@/components/settings/appearance-section";
import { ComingSoon } from "@/components/settings/coming-soon";
import {
  getSectionMeta,
  type SettingsSectionId,
} from "@/components/settings/registry";

/**
 * Routes the active section id to its content body. Sections not yet built fall
 * through to <ComingSoon> so the nav is fully populated day one (SET.1a).
 */
export function SettingsSectionContent({ id }: { id: SettingsSectionId }) {
  switch (id) {
    case "appearance":
      return <AppearanceSection />;
    default:
      return <ComingSoon label={getSectionMeta(id).label} />;
  }
}
