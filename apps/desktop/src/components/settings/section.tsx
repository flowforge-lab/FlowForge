import { lazy, Suspense } from "react";
import { ComingSoon } from "@/components/settings/coming-soon";
import {
  getSectionMeta,
  type SettingsSectionId,
} from "@/components/settings/registry";

// Code-split each settings section into its own async chunk (#599 item 6). These
// sections (scheduled's cron builder, memory's chunk views, the MCP panel, …) are
// only reachable once the user opens Settings, so keeping them out of the initial
// bundle means the first-paint JS parse covers only the chat surface. Named
// exports are adapted to the default-export shape `lazy` expects.
const AppearanceSection = lazy(() =>
  import("@/components/settings/appearance-section").then((m) => ({
    default: m.AppearanceSection,
  })),
);
const ControlSection = lazy(() =>
  import("@/components/settings/control-section").then((m) => ({
    default: m.ControlSection,
  })),
);
const McpSection = lazy(() =>
  import("@/components/settings/mcp/mcp-section").then((m) => ({
    default: m.McpSection,
  })),
);
const MemorySection = lazy(() =>
  import("@/components/settings/memory-section").then((m) => ({
    default: m.MemorySection,
  })),
);
const ModelSection = lazy(() =>
  import("@/components/settings/model-section").then((m) => ({
    default: m.ModelSection,
  })),
);
const SkillsSection = lazy(() =>
  import("@/components/settings/skills-section").then((m) => ({
    default: m.SkillsSection,
  })),
);
const ProfilesSection = lazy(() =>
  import("@/components/settings/profiles-section").then((m) => ({
    default: m.ProfilesSection,
  })),
);
const KeyboardSection = lazy(() =>
  import("@/components/settings/keyboard-section").then((m) => ({
    default: m.KeyboardSection,
  })),
);
const ScheduledSection = lazy(() =>
  import("@/components/settings/scheduled-section").then((m) => ({
    default: m.ScheduledSection,
  })),
);
const ExperimentalSection = lazy(() =>
  import("@/components/settings/experimental-section").then((m) => ({
    default: m.ExperimentalSection,
  })),
);
const AboutSection = lazy(() =>
  import("@/components/settings/about-section").then((m) => ({
    default: m.AboutSection,
  })),
);

/** Body of the section while its async chunk loads. Local files load fast, but a
 *  fallback keeps the layout stable and avoids a blank flash. */
function SectionLoading() {
  return (
    <div
      className="p-4 text-sm text-muted-foreground"
      role="status"
      aria-live="polite"
    >
      Loading…
    </div>
  );
}

function sectionBody(id: SettingsSectionId) {
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

/**
 * Routes the active section id to its content body. Sections not yet built fall
 * through to <ComingSoon> so the nav is fully populated day one (SET.1a). Each
 * built section is lazy-loaded (#599 item 6); Suspense shows a light fallback
 * while its chunk resolves.
 */
export function SettingsSectionContent({ id }: { id: SettingsSectionId }) {
  return <Suspense fallback={<SectionLoading />}>{sectionBody(id)}</Suspense>;
}
