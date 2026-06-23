import { useState } from "react";
import { Plus, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { SettingsSwitch } from "@/components/settings/switch";
import { useControlConfigStore } from "@/store/control-config";

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

  if (!config) return null;

  const submitFile = () => {
    if (newFile.trim() === "") return;
    void addPromptFile(newFile);
    setNewFile("");
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
          // Commit on blur (not each keystroke) — this is file-backed
          // (user_instructions.md) once the backend lands. `key` resyncs the
          // field if the config is replaced externally (e.g. reset).
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
            onChange={(e) => setNewFile(e.target.value)}
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
        </div>

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
