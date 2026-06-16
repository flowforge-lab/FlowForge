import { ThemeSettings } from "@/components/theme-settings";
import { SearchSettings } from "@/components/search-settings";

/**
 * Appearance section — hosts the existing theme/font picker and the web-search
 * backend picker, migrated verbatim from the old slide-over so nothing regresses.
 */
export function AppearanceSection() {
  return (
    <div>
      <ThemeSettings />
      <SearchSettings />
    </div>
  );
}
