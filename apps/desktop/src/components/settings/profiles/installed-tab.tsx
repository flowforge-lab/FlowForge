import { useState } from "react";
import { FolderPlus, Lock, Star } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  defaultProfileId,
  useProfilesStore,
  type Profile,
  type ProfileAccent,
} from "@/store/profiles";

// Static accent → left-border class (Tailwind can't see dynamic class strings).
const ACCENT_BORDER: Record<ProfileAccent, string> = {
  blue: "border-l-blue-500",
  violet: "border-l-violet-500",
  emerald: "border-l-emerald-500",
  amber: "border-l-amber-500",
  rose: "border-l-rose-500",
};

/**
 * Installed sub-tab (SET.7): profile cards (name, description, skill count, lock
 * icon, accent border, ACTIVE badge, default star) plus "Install local profile…".
 * Selecting a card switches the active phenotype via `useProfilesStore`. Local
 * install isn't wired — the button surfaces that it arrives with the backend.
 */
export function InstalledTab() {
  const profiles = useProfilesStore((s) => s.profiles);
  const activeId = useProfilesStore((s) => s.activeId);
  const loading = useProfilesStore((s) => s.loading);
  const saving = useProfilesStore((s) => s.saving);
  const error = useProfilesStore((s) => s.error);
  const setActive = useProfilesStore((s) => s.setActive);
  const [installHint, setInstallHint] = useState(false);
  const defaultId = defaultProfileId(profiles);

  return (
    <div className="space-y-5">
      <section className="space-y-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setInstallHint(true)}
        >
          <FolderPlus />
          Install local profile…
        </Button>
        {installHint ? (
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            Installing a profile from a local folder arrives with the backend
            installer. For now, switch profiles via the ⌘K palette.
          </p>
        ) : null}
      </section>

      <section className="space-y-2 border-t pt-5">
        {error ? (
          <p className="text-[12px] text-destructive" role="alert">
            {error}
          </p>
        ) : null}

        {loading && profiles.length === 0 ? (
          <p className="text-[12px] text-muted-foreground">Loading profiles…</p>
        ) : profiles.length === 0 ? (
          <p className="text-[12px] text-muted-foreground">
            No profiles installed.
          </p>
        ) : (
          <ul className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {profiles.map((profile) => (
              <ProfileCard
                key={profile.id}
                profile={profile}
                active={profile.id === activeId}
                isDefault={profile.id === defaultId}
                disabled={saving}
                onSelect={() => void setActive(profile.id)}
              />
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function ProfileCard({
  profile,
  active,
  isDefault,
  disabled,
  onSelect,
}: {
  profile: Profile;
  active: boolean;
  isDefault: boolean;
  disabled: boolean;
  onSelect: () => void;
}) {
  return (
    <li>
      <button
        type="button"
        aria-pressed={active}
        disabled={disabled}
        onClick={onSelect}
        className={cn(
          "flex w-full flex-col gap-1.5 rounded-md border border-l-2 px-3 py-2.5 text-left transition-colors outline-none",
          ACCENT_BORDER[profile.accent],
          "hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-primary/30",
          "disabled:cursor-not-allowed disabled:opacity-60",
          active && "bg-primary/5 ring-2 ring-primary/30",
        )}
      >
        <div className="flex items-center gap-1.5">
          <span className="min-w-0 truncate text-[13px] font-medium text-foreground">
            {profile.name}
          </span>
          {isDefault ? (
            <Star
              className="size-3 shrink-0 fill-amber-400 text-amber-400"
              aria-label="Default profile"
            />
          ) : null}
          {profile.locked ? (
            <Lock
              className="size-3 shrink-0 text-muted-foreground"
              aria-label="Locked"
            />
          ) : null}
          {active ? (
            <span className="ml-auto shrink-0 rounded-full bg-primary/10 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-primary">
              Active
            </span>
          ) : null}
        </div>
        <p className="line-clamp-2 text-[11px] leading-relaxed text-muted-foreground">
          {profile.description}
        </p>
        <span className="text-[10px] tabular-nums text-muted-foreground">
          {profile.skillCount} skill{profile.skillCount === 1 ? "" : "s"}
        </span>
      </button>
    </li>
  );
}
