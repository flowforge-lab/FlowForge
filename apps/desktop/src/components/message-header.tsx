import { usePrefsStore } from "@/store/prefs";
import { useProfilesStore } from "@/store/profiles";
import { formatMessageTime } from "@/lib/format-message-time";

/**
 * Author + timestamp line above a chat message (#641): `<name>  <time>`, name in
 * medium weight, time muted to its right. Reads the author name from the stores by
 * role so chat-view stays a two-line change and headers update live:
 * - user → `prefs.displayName`, falling back to "You" when blank.
 * - assistant → the active phenotype display name, falling back to "Assistant".
 *
 * The assistant name reflects the *currently* active phenotype, not the one that
 * produced each message: a thread reloaded after the user switches phenotype will
 * re-label past assistant turns with the new name. A faithful per-message author
 * needs a `Message` author field (schema + bindings), tracked as a follow-up (#657).
 *
 * The parent message column (`items-end` for user, `items-start` for assistant)
 * handles left/right alignment. The time span is omitted when `createdAt` has no
 * usable timestamp (see `formatMessageTime`).
 */
export function MessageHeader({
  role,
  createdAt,
}: {
  role: "user" | "assistant";
  createdAt: number;
}) {
  const displayName = usePrefsStore((s) => s.displayName);
  const activeId = useProfilesStore((s) => s.activeId);
  const profiles = useProfilesStore((s) => s.profiles);

  const name =
    role === "user"
      ? displayName || "You"
      : (profiles.find((p) => p.id === activeId)?.name ?? "Assistant");

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
