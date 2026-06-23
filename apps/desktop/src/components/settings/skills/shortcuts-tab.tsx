import { useState } from "react";
import { Plus, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  normalizeShortcutName,
  useCommandShortcutsStore,
} from "@/store/command-shortcuts";

/**
 * Shortcuts sub-tab (SET.5): create `/name` message shortcuts that send a canned
 * message when invoked (NOT system-prompt injection, and distinct from the GLOBAL
 * "Keyboard" section's key bindings). Persisted via `useCommandShortcutsStore`.
 */
export function ShortcutsTab() {
  const shortcuts = useCommandShortcutsStore((s) => s.shortcuts);
  const addShortcut = useCommandShortcutsStore((s) => s.addShortcut);
  const removeShortcut = useCommandShortcutsStore((s) => s.removeShortcut);

  const [name, setName] = useState("");
  const [message, setMessage] = useState("");

  const cleanName = normalizeShortcutName(name);
  const duplicate = shortcuts.some(
    (s) => s.name.toLowerCase() === cleanName.toLowerCase(),
  );
  const canCreate = cleanName !== "" && message.trim() !== "" && !duplicate;

  const create = () => {
    if (!addShortcut(name, message)) return;
    setName("");
    setMessage("");
  };

  return (
    <div className="space-y-5">
      <section className="space-y-3">
        <h3 className="text-[13px] font-medium text-foreground">
          Create a Shortcut
        </h3>
        <div className="space-y-1.5">
          <label
            htmlFor="shortcut-name"
            className="text-[11px] font-medium text-muted-foreground"
          >
            Name
          </label>
          <div className="flex items-center gap-1.5">
            <span className="text-[13px] text-muted-foreground">/</span>
            <Input
              id="shortcut-name"
              value={name}
              placeholder="ship"
              autoComplete="off"
              aria-invalid={duplicate || undefined}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          {duplicate ? (
            <p className="text-[11px] text-destructive">
              A shortcut named “/{cleanName}” already exists.
            </p>
          ) : null}
        </div>

        <div className="space-y-1.5">
          <label
            htmlFor="shortcut-message"
            className="text-[11px] font-medium text-muted-foreground"
          >
            Message
          </label>
          <Textarea
            id="shortcut-message"
            value={message}
            rows={3}
            placeholder="Open a PR for the current branch following CONTRIBUTING."
            className="min-h-16 text-[13px]"
            onChange={(e) => setMessage(e.target.value)}
          />
        </div>

        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canCreate}
          onClick={create}
        >
          <Plus />
          Create
        </Button>
      </section>

      <section className="space-y-2 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">
          Your shortcuts
        </h3>
        {shortcuts.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            No shortcuts yet. Send a saved message by typing{" "}
            <code className="text-[10px]">/name</code> in the composer.
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {shortcuts.map((s) => (
              <li
                key={s.id}
                className="flex items-start gap-2.5 rounded-md bg-muted/40 px-2.5 py-2"
              >
                <div className="min-w-0 flex-1">
                  <code className="text-[12px] font-medium text-foreground">
                    /{s.name}
                  </code>
                  <p className="truncate text-[11px] text-muted-foreground">
                    {s.message}
                  </p>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="text-muted-foreground hover:text-destructive"
                  onClick={() => removeShortcut(s.id)}
                  title={`Remove /${s.name}`}
                >
                  <X />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
