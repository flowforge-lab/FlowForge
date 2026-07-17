import { useState } from "react";
import { Folder, Plus, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { SettingsSwitch } from "@/components/settings/switch";
import { useControlConfigStore } from "@/store/control-config";

/** The desktop shell exposes Tauri internals; absent under `VITE_FF_MOCK` in a plain
 *  browser tab, where the native file dialog and fs checks can't run. */
const IN_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Open the native picker for one or more prompt files; `[]` on cancel. */
async function browseFiles(): Promise<string[]> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ multiple: true });
  if (picked === null) return [];
  return Array.isArray(picked) ? picked : [picked];
}

/** Light existence check under Tauri; `true` (don't warn) if it can't be checked. */
async function pathExists(path: string): Promise<boolean> {
  try {
    const { exists } = await import("@tauri-apps/plugin-fs");
    return await exists(path);
  } catch {
    return true;
  }
}

/** Inline feedback after an add attempt: paths already listed, and (under Tauri)
 *  paths that don't exist on disk but were still added. */
interface AddNotice {
  dupes: string[];
  missing: string[];
}

/** Prompts sub-tab: inject-memory switch, user instructions, extra prompt files. */
export function PromptsTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const setInjectMemory = useControlConfigStore((s) => s.setInjectMemory);
  const setUserInstructions = useControlConfigStore(
    (s) => s.setUserInstructions,
  );
  const addPromptFile = useControlConfigStore((s) => s.addPromptFile);
  const removePromptFile = useControlConfigStore((s) => s.removePromptFile);

  const [newFile, setNewFile] = useState("");
  const [notice, setNotice] = useState<AddNotice | null>(null);

  if (!config) return null;

  // Add one or more candidate paths: skip blanks, flag duplicates instead of
  // silently dropping them, and (under Tauri) warn about paths that don't exist
  // while still adding them. Duplicate detection happens here — the component
  // already has the list, so the store stays untouched.
  const addPaths = async (paths: string[]) => {
    const existing = new Set(config.promptFiles);
    const dupes: string[] = [];
    const added: string[] = [];
    for (const raw of paths) {
      const trimmed = raw.trim();
      if (trimmed === "") continue;
      if (existing.has(trimmed) || added.includes(trimmed)) {
        if (!dupes.includes(trimmed)) dupes.push(trimmed);
        continue;
      }
      added.push(trimmed);
      void addPromptFile(trimmed);
    }

    const missing = IN_TAURI
      ? (await Promise.all(added.map((p) => pathExists(p))))
          .map((ok, i) => (ok ? null : added[i]))
          .filter((p): p is string => p !== null)
      : [];

    setNotice(dupes.length || missing.length ? { dupes, missing } : null);
  };

  const submitFile = () => {
    if (newFile.trim() === "") return;
    void addPaths([newFile]);
    setNewFile("");
  };

  const onBrowse = async () => {
    const picked = await browseFiles();
    if (picked.length > 0) await addPaths(picked);
  };

  return (
    <div className="space-y-5">
      <SettingsSwitch
        label="Inject memory"
        description="Prepend saved memory to the system prompt."
        checked={config.injectMemory}
        disabled={saving}
        onCheckedChange={(inject) => void setInjectMemory(inject)}
      />

      <section className="space-y-1.5 border-t pt-5">
        <label
          htmlFor="user-instructions"
          className="text-[13px] font-medium text-foreground"
        >
          User instructions
        </label>
        <Textarea
          id="user-instructions"
          // Commit on blur (not each keystroke). The backend injects these
          // instructions into the volatile tail of the system prompt every turn
          // (#1002). `key` resyncs the field if the config is replaced
          // externally (e.g. reset).
          key={config.userInstructions}
          rows={5}
          defaultValue={config.userInstructions}
          placeholder="Always-on instructions added to every conversation…"
          className="min-h-28 font-sans text-[13px]"
          disabled={saving}
          onBlur={(e) => {
            if (e.target.value !== config.userInstructions) {
              void setUserInstructions(e.target.value);
            }
          }}
        />
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          Stored in <code className="text-[10px]">user_instructions.md</code>.
        </p>
      </section>

      <section className="space-y-2 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">
          Additional prompt files
        </h3>
        <div className="flex items-center gap-1.5">
          <Input
            value={newFile}
            placeholder="{workspace}/AGENTS.md"
            autoComplete="off"
            disabled={saving}
            onChange={(e) => {
              setNewFile(e.target.value);
              if (notice) setNotice(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                submitFile();
              }
            }}
          />
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={saving || newFile.trim() === ""}
            onClick={submitFile}
          >
            <Plus />
            Add
          </Button>
          {IN_TAURI ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="shrink-0"
              disabled={saving}
              onClick={() => void onBrowse()}
            >
              <Folder />
              Browse
            </Button>
          ) : null}
        </div>

        {notice ? (
          <div className="space-y-0.5 text-[11px] leading-relaxed">
            {notice.dupes.length > 0 ? (
              <p className="text-muted-foreground">
                Already in the list:{" "}
                <code className="text-[10px]">{notice.dupes.join(", ")}</code>
              </p>
            ) : null}
            {notice.missing.length > 0 ? (
              <p className="text-amber-600 dark:text-amber-400">
                Not found (added anyway):{" "}
                <code className="text-[10px]">{notice.missing.join(", ")}</code>
              </p>
            ) : null}
          </div>
        ) : null}

        {config.promptFiles.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            No extra prompt files.
          </p>
        ) : (
          <ul className="flex flex-col gap-1">
            {config.promptFiles.map((path) => (
              <li
                key={path}
                className="flex items-center gap-2 rounded-md bg-muted/50 px-2 py-1"
              >
                <code className="min-w-0 flex-1 truncate text-[11px] text-foreground">
                  {path}
                </code>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  className="text-muted-foreground hover:text-destructive"
                  disabled={saving}
                  onClick={() => void removePromptFile(path)}
                  title={`Remove ${path}`}
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
