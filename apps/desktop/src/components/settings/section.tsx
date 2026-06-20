import { AppearanceSection } from "@/components/settings/appearance-section";
import { ControlSection } from "@/components/settings/control-section";
import { McpSection } from "@/components/settings/mcp/mcp-section";
import { MemorySection } from "@/components/settings/memory-section";
import { ModelSection } from "@/components/settings/model-section";
import { SkillsSection } from "@/components/settings/skills-section";
import { ProfilesSection } from "@/components/settings/profiles-section";
import { KeyboardSection } from "@/components/settings/keyboard-section";
import { ScheduledSection } from "@/components/settings/scheduled-section";
import { ExperimentalSection } from "@/components/settings/experimental-section";
import { AboutSection } from "@/components/settings/about-section";
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
    case "model":
      return <ModelSection />;
    case "skills":
      return <SkillsSection />;
    case "profiles":
      return <ProfilesSection />;
    case "control":
      return <ControlSection />;
    case "mcp":
      return <McpSection />;
    case "memory":
      return <MemorySection />;
    case "keyboard":
      return <KeyboardSection />;
    case "scheduled":
      return <ScheduledSection />;
    case "experimental":
      return <ExperimentalSection />;
    case "about":
      return <AboutSection />;
    default:
      return <ComingSoon label={getSectionMeta(id).label} />;
  }
}
