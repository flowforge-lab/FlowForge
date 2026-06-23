import { useEffect, useState } from "react";
import { ChevronRight, FolderPlus, Package } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useSkillsStore } from "@/store/skills";

type LoadState = "loading" | "ready" | "error";

/**
 * Installed sub-tab (SET.5): "Install local skill…" plus a read-only **Bundled**
 * group (the skills shipped with the app) with a count badge, expandable. Sourced
 * from `useSkillsStore` (`listSkills` / `SkillInfo`); local install isn't wired
 * yet — the button surfaces that it arrives with the backend installer.
 */
export function InstalledTab() {
  const skills = useSkillsStore((s) => s.skills);
  const refresh = useSkillsStore((s) => s.refresh);
  const [state, setState] = useState<LoadState>("loading");
  const [expanded, setExpanded] = useState(true);
  const [installHint, setInstallHint] = useState(false);

  useEffect(() => {
    // Initial state is already "loading"; only the async resolution updates it,
    // so no synchronous setState in the effect body.
    let alive = true;
    refresh()
      .then(() => alive && setState("ready"))
      .catch(() => alive && setState("error"));
    return () => {
      alive = false;
    };
  }, [refresh]);

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
          Install local skill…
        </Button>
        {installHint ? (
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            Installing a skill from a local folder arrives with the backend
            installer. For now, manage skills via the ⌘K palette.
          </p>
        ) : null}
      </section>

      <section className="space-y-2 border-t pt-5">
        <button
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
          className="flex w-full items-center gap-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
        >
          <ChevronRight
            className={cn(
              "size-4 shrink-0 text-muted-foreground transition-transform",
              expanded && "rotate-90",
            )}
          />
          <span className="text-[13px] font-medium text-foreground">
            Bundled
          </span>
          <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] font-medium tabular-nums text-muted-foreground">
            {skills.length}
          </span>
          <span className="ml-auto text-[11px] text-muted-foreground">
            Read-only
          </span>
        </button>

        {expanded ? <BundledBody state={state} skills={skills} /> : null}
      </section>
    </div>
  );
}

function BundledBody({
  state,
  skills,
}: {
  state: LoadState;
  skills: ReturnType<typeof useSkillsStore.getState>["skills"];
}) {
  if (state === "loading") {
    return <p className="text-[12px] text-muted-foreground">Loading skills…</p>;
  }
  if (state === "error") {
    return (
      <p className="text-[12px] text-destructive" role="alert">
        Couldn’t load installed skills.
      </p>
    );
  }
  if (skills.length === 0) {
    return (
      <p className="text-[12px] text-muted-foreground">
        No bundled skills found.
      </p>
    );
  }
  return (
    <ul className="flex flex-col gap-1.5">
      {skills.map((skill) => (
        <li
          key={skill.name}
          className="flex items-start gap-2.5 rounded-md bg-muted/40 px-2.5 py-2"
        >
          <Package className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span className="truncate text-[13px] font-medium text-foreground">
                {skill.name}
              </span>
              <span className="shrink-0 text-[10px] tabular-nums text-muted-foreground">
                v{skill.version}
              </span>
            </div>
            <p className="truncate text-[11px] text-muted-foreground">
              {skill.description}
            </p>
          </div>
        </li>
      ))}
    </ul>
  );
}
