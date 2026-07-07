import { useState } from "react";
import { Pencil, Plus, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { slugify } from "@/lib/control";
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

/** Team sub-tab (SET.12, #805): teammate profiles list + an add/edit form. */
export function TeamTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const addTeammate = useControlConfigStore((s) => s.addTeammate);
  const updateTeammate = useControlConfigStore((s) => s.updateTeammate);
  const removeTeammate = useControlConfigStore((s) => s.removeTeammate);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [description, setDescription] = useState("");
  const [attempted, setAttempted] = useState(false);

  if (!config) return null;

  const isEditing = editingId !== null;
  // The slug the store will persist: the typed handle, or one derived from the name.
  const derivedSlug = slugify(slug) || slugify(name);
  const nameEmpty = name.trim() === "";
  const slugTaken =
    derivedSlug !== "" &&
    config.teammates.some((t) => t.slug === derivedSlug && t.id !== editingId);
  const canSubmit = !nameEmpty && !slugTaken;

  const resetForm = () => {
    setEditingId(null);
    setName("");
    setSlug("");
    setDescription("");
    setAttempted(false);
  };

  const startEdit = (id: string) => {
    const t = config.teammates.find((tm) => tm.id === id);
    if (!t) return;
    setEditingId(t.id);
    setName(t.name);
    setSlug(t.slug);
    setDescription(t.description);
    setAttempted(false);
  };

  const submit = () => {
    setAttempted(true);
    if (!canSubmit) return;
    if (isEditing) void updateTeammate(editingId, { name, slug, description });
    else void addTeammate({ name, slug, description });
    resetForm();
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
                className={`flex items-start gap-2.5 rounded-md px-2.5 py-2 ${
                  editingId === t.id
                    ? "bg-muted/60 ring-1 ring-primary/40"
                    : "bg-muted/40"
                }`}
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
                <div className="flex shrink-0 items-center">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="text-muted-foreground hover:text-foreground"
                    disabled={saving}
                    onClick={() => startEdit(t.id)}
                    title={`Edit ${t.name}`}
                    aria-label={`Edit ${t.name}`}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="text-muted-foreground hover:text-destructive"
                    disabled={saving}
                    onClick={() => {
                      if (editingId === t.id) resetForm();
                      void removeTeammate(t.id);
                    }}
                    title={`Remove ${t.name}`}
                    aria-label={`Remove ${t.name}`}
                  >
                    <X />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-3 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">
          {isEditing ? "Edit teammate" : "Add teammate"}
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
              aria-invalid={attempted && nameEmpty}
              disabled={saving}
              onChange={(e) => setName(e.target.value)}
            />
            {attempted && nameEmpty ? (
              <p className="text-[11px] text-destructive">Name is required.</p>
            ) : null}
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
              aria-invalid={slugTaken}
              disabled={saving}
              onChange={(e) => setSlug(e.target.value)}
            />
            {slugTaken ? (
              <p className="text-[11px] text-destructive">
                @{derivedSlug} is already taken.
              </p>
            ) : slug.trim() === "" && derivedSlug !== "" ? (
              <p className="text-[11px] text-muted-foreground">
                Will use @{derivedSlug}.
              </p>
            ) : null}
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
        <div className="flex items-center gap-1.5">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={(attempted && !canSubmit) || saving}
            onClick={submit}
          >
            {isEditing ? <Pencil /> : <Plus />}
            {isEditing ? "Save" : "Add teammate"}
          </Button>
          {isEditing ? (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={saving}
              onClick={resetForm}
            >
              Cancel
            </Button>
          ) : null}
        </div>
      </section>
    </div>
  );
}
