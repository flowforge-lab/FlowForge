import { SearchSettings } from "@/components/search-settings";
import { SettingsSlider } from "@/components/settings/slider";
import {
  OPEN_THREADS_MAX,
  OPEN_THREADS_MIN,
  usePrefsStore,
} from "@/store/prefs";

/**
 * Advanced sub-tab: the open-thread budget and the web-search backend picker
 * (kept reachable here per SET.2). Open-threads is an FE flag until the backend
 * enforces the LRU.
 */
export function AdvancedTab() {
  const openThreads = usePrefsStore((s) => s.openThreads);
  const setOpenThreads = usePrefsStore((s) => s.setOpenThreads);

  return (
    <div>
      <section className="space-y-1.5">
        <SettingsSlider
          label="Open threads"
          value={openThreads}
          onValueChange={setOpenThreads}
          min={OPEN_THREADS_MIN}
          max={OPEN_THREADS_MAX}
          step={1}
        />
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          Maximum threads kept loaded at once; least-recently-used threads are
          unloaded past this limit.
        </p>
      </section>

      {/* SearchSettings brings its own `mt-8 border-t pt-5` separator. */}
      <SearchSettings />
    </div>
  );
}
