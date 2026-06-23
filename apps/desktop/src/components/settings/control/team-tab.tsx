import { useState } from "react";
import { Plus, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useControlConfigStore } from "@/store/control-config";

/** Initials for the avatar fallback (first letters of the first two words). */
function initials(name: string): string {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");
}

/** Team sub-tab (SET.12): teammate profiles list + a stub "Add teammate" form. */
export function TeamTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const addTeammate = useControlConfigStore((s) => s.addTeammate);
  const removeTeammate = useControlConfigStore((s) => s.removeTeammate);

  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [description, setDescription] = useState("");

  if (!config) return null;

  const canAdd = name.trim() !== "";

  const submit = () => {
    if (!canAdd) return;
    void addTeammate({ name, slug, description });
    setName("");
    setSlug("");
    setDescription("");
  };

  return (
    <div className="space-y-5">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">Teammates</h3>
        {config.teammates.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            No teammates yet. Add one below.
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {config.teammates.map((t) => (
              <li
                key={t.id}
                className="flex items-start gap-2.5 rounded-md bg-muted/40 px-2.5 py-2"
              >
                <span
                  className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full bg-primary/10 text-[11px] font-semibold text-primary"
                  aria-hidden
                >
                  {initials(t.name)}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-baseline gap-2">
                    <span className="truncate text-[13px] font-medium text-foreground">
                      {t.name}
                    </span>
                    {t.slug ? (
                      <span className="shrink-0 text-[11px] text-muted-foreground">
                        @{t.slug}
                      </span>
                    ) : null}
                  </div>
                  {t.description ? (
                    <p className="text-[11px] leading-relaxed text-muted-foreground">
                      {t.description}
                    </p>
                  ) : null}
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="text-muted-foreground hover:text-destructive"
                  disabled={saving}
                  onClick={() => void removeTeammate(t.id)}
                  title={`Remove ${t.name}`}
                  aria-label={`Remove ${t.name}`}
                >
                  <X />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-3 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">
          Add teammate
        </h3>
        <div className="grid grid-cols-2 gap-2">
          <div className="space-y-1.5">
            <label
              htmlFor="teammate-name"
              className="text-[11px] font-medium text-muted-foreground"
            >
              Name
            </label>
            <Input
              id="teammate-name"
              value={name}
              placeholder="Riley Reviewer"
              autoComplete="off"
              disabled={saving}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <label
              htmlFor="teammate-slug"
              className="text-[11px] font-medium text-muted-foreground"
            >
              Slug
            </label>
            <Input
              id="teammate-slug"
              value={slug}
              placeholder="reviewer"
              autoComplete="off"
              disabled={saving}
              onChange={(e) => setSlug(e.target.value)}
            />
          </div>
        </div>
        <div className="space-y-1.5">
          <label
            htmlFor="teammate-description"
            className="text-[11px] font-medium text-muted-foreground"
          >
            Description
          </label>
          <Textarea
            id="teammate-description"
            value={description}
            rows={2}
            placeholder="What this teammate is good at…"
            className="min-h-12 text-[13px]"
            disabled={saving}
            onChange={(e) => setDescription(e.target.value)}
          />
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canAdd || saving}
          onClick={submit}
        >
          <Plus />
          Add teammate
        </Button>
      </section>
    </div>
  );
}
