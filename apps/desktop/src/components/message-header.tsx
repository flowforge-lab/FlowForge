import { usePrefsStore } from "@/store/prefs";
import { phenotypeDisplayName, useProfilesStore } from "@/store/profiles";
import { formatMessageTime } from "@/lib/format-message-time";

/**
 * Author + timestamp line above a chat message (#641): `<name>  <time>`, name in
 * medium weight, time muted to its right. Reads the author name by role so
 * chat-view stays a two-line change and headers update live:
 * - user → `prefs.displayName`, falling back to "You" when blank.
 * - assistant → the persisted authoring phenotype (#657), falling back to the
 *   currently active phenotype for rows written before authors were stored.
 *
 * `authorName` is the raw phenotype name (the profile id) stamped on the message
 * when the turn ran. Preferring it means a thread reloaded after the user switches
 * phenotype keeps each past turn labeled with the phenotype that produced it,
 * rather than relabeling the whole history with the new active phenotype. A
 * persisted id whose profile no longer exists is title-cased for display; a row
 * with no stored author (older messages) falls back to the live active phenotype.
 *
 * The parent message column (`items-end` for user, `items-start` for assistant)
 * handles left/right alignment. The time span is omitted when `createdAt` has no
 * usable timestamp (see `formatMessageTime`).
 */
export function MessageHeader({
  role,
  createdAt,
  authorName,
}: {
  role: "user" | "assistant";
  createdAt: number;
  authorName?: string | null;
}) {
  const displayName = usePrefsStore((s) => s.displayName);
  const activeId = useProfilesStore((s) => s.activeId);
  const profiles = useProfilesStore((s) => s.profiles);

  let name: string;
  if (role === "user") {
    name = displayName || "You";
  } else if (authorName) {
    // Persisted historical author: resolve to its current display name, or
    // title-case the raw phenotype name when its profile no longer exists.
    name =
      profiles.find((p) => p.id === authorName)?.name ??
      phenotypeDisplayName(authorName);
  } else {
    // Pre-authors rows: fall back to the live active phenotype.
    name = profiles.find((p) => p.id === activeId)?.name ?? "Assistant";
  }

  const time = formatMessageTime(createdAt);

  return (
    <div className="flex items-baseline gap-2 px-0.5">
      <span className="text-[11px] font-medium text-muted-foreground">
        {name}
      </span>
      {time ? (
        <span className="text-[11px] tabular-nums text-muted-foreground/70">
          {time}
        </span>
      ) : null}
    </div>
  );
}
