import { useEffect, useMemo, useState } from "react";
import { Folder } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SettingsSwitch } from "@/components/settings/switch";
import {
  ScheduleBuilder,
  type CadencePreview,
} from "@/components/settings/schedule-builder";
import { ipc } from "@/lib/ipc";
import {
  builderToCron,
  cronToBuilder,
  DEFAULT_BUILDER_STATE,
  type ScheduleBuilderState,
} from "@/lib/schedule-cron";
import { useScheduledStore } from "@/store/scheduled";
import { useProfilesStore } from "@/store/profiles";
import type { CreateScheduledTaskInput, ScheduledTask } from "@/bindings";

/** Sentinel `Select` value for "inherit the active profile" (radix needs a value). */
const INHERIT = "__inherit__";

/** The desktop shell exposes Tauri internals; absent under `VITE_FF_MOCK` in a plain
 *  browser tab, where the native folder dialog can't run. */
const IN_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** A quick-start preset that prefills the form (#541). */
interface QuickStart {
  id: string;
  label: string;
  name: string;
  instructions: string;
  schedule: ScheduleBuilderState;
}

const QUICK_STARTS: ReadonlyArray<QuickStart> = [
  {
    id: "daily-digest",
    label: "Daily digest",
    name: "Daily Digest",
    instructions:
      "Summarize what changed in my workspace today and list anything that needs my attention.",
    schedule: { ...DEFAULT_BUILDER_STATE, cadence: "daily", time: "17:00" },
  },
  {
    id: "evening-reflection",
    label: "Evening reflection",
    name: "Evening Reflection",
    instructions:
      "Reflect on today's progress and note open threads to pick up tomorrow.",
    schedule: { ...DEFAULT_BUILDER_STATE, cadence: "daily", time: "21:00" },
  },
  {
    id: "cr-review-check",
    label: "CR review check",
    name: "CR Review Check",
    instructions:
      "Check for open pull requests awaiting my review and summarize them.",
    schedule: {
      ...DEFAULT_BUILDER_STATE,
      cadence: "weekly",
      time: "09:00",
      weeklyOn: "weekdays",
    },
  },
];

async function browseFolder(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false });
  return typeof picked === "string" ? picked : null;
}

function fieldLabelClass() {
  return "text-[11px] font-semibold tracking-wide text-muted-foreground uppercase";
}

/**
 * New/Edit form for a scheduled task (#541, RFC 0017). Collects name, instructions,
 * workspace, profile, and safety ceiling, embeds the schedule builder, and submits
 * through the scheduled store (`create`, or delete-then-recreate `edit`). Seeded from
 * `task` when editing; quick-start chips prefill on a new task.
 */
export function ScheduledTaskForm({
  task,
  onDone,
}: {
  task?: ScheduledTask;
  onDone: () => void;
}) {
  const create = useScheduledStore((s) => s.create);
  const edit = useScheduledStore((s) => s.edit);
  const saving = useScheduledStore((s) => s.saving);
  const profiles = useProfilesStore((s) => s.profiles);
  const loadProfiles = useProfilesStore((s) => s.load);

  const isEdit = task !== undefined;

  const [name, setName] = useState(task?.name ?? "");
  const [instructions, setInstructions] = useState(
    task && task.kind.kind === "prompt" ? task.kind.value : "",
  );
  const [workspace, setWorkspace] = useState(task?.workspace ?? "");
  const [profile, setProfile] = useState(task?.profile ?? INHERIT);
  const [allowWrite, setAllowWrite] = useState(task?.safetyCeiling === "write");
  const [schedule, setSchedule] = useState<ScheduleBuilderState>(
    task ? cronToBuilder(task.cron) : DEFAULT_BUILDER_STATE,
  );

  // Profiles power the Profile dropdown; load lazily when the form opens.
  useEffect(() => {
    if (profiles.length === 0) void loadProfiles();
  }, [profiles.length, loadProfiles]);

  // Live cadence preview, derived from the composed cron via the backend (the one
  // source of truth for labels). Debounced so each keystroke in Custom doesn't probe.
  const cron = useMemo(() => builderToCron(schedule), [schedule]);
  const [preview, setPreview] = useState<CadencePreview | null>(null);
  useEffect(() => {
    let alive = true;
    // Defer every state set into the debounce so nothing runs synchronously during
    // the effect (and Custom keystrokes don't probe the backend on every change).
    const handle = setTimeout(async () => {
      if (!cron) {
        if (alive)
          setPreview({ state: "error", message: "Enter a cron expression." });
        return;
      }
      if (alive) setPreview({ state: "loading" });
      try {
        const label = await ipc.previewCadence(cron);
        if (alive) setPreview({ state: "ok", label });
      } catch (err) {
        if (alive)
          setPreview({
            state: "error",
            message: err instanceof Error ? err.message : String(err),
          });
      }
    }, 300);
    return () => {
      alive = false;
      clearTimeout(handle);
    };
  }, [cron]);

  const cadenceValid = preview?.state === "ok";
  const canSubmit =
    name.trim() !== "" && instructions.trim() !== "" && cadenceValid && !saving;

  const applyQuickStart = (q: QuickStart) => {
    setName(q.name);
    setInstructions(q.instructions);
    setSchedule(q.schedule);
  };

  const onBrowse = async () => {
    const picked = await browseFolder();
    if (picked) setWorkspace(picked);
  };

  const submit = async () => {
    if (!canSubmit) return;
    const input: CreateScheduledTaskInput = {
      name: name.trim(),
      cron,
      kind: { kind: "prompt", value: instructions.trim() },
      workspace: workspace.trim() || undefined,
      profile: profile === INHERIT ? undefined : profile,
      safetyCeiling: allowWrite ? "write" : "read_only",
    };
    if (isEdit) await edit(task.id, input);
    else await create(input);
    onDone();
  };

  return (
    <div className="space-y-4 rounded-md border bg-muted/30 p-3.5">
      {!isEdit ? (
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="mr-1 text-[11px] text-muted-foreground">
            Quick start:
          </span>
          {QUICK_STARTS.map((q) => (
            <button
              key={q.id}
              type="button"
              onClick={() => applyQuickStart(q)}
              className="rounded-full border border-border bg-background px-2.5 py-1 text-[11px] font-medium text-muted-foreground transition-colors hover:border-primary/40 hover:text-foreground"
            >
              {q.label}
            </button>
          ))}
        </div>
      ) : null}

      <div className="space-y-1.5">
        <label htmlFor="task-name" className={fieldLabelClass()}>
          Name
        </label>
        <Input
          id="task-name"
          value={name}
          placeholder="Daily Digest"
          autoComplete="off"
          onChange={(e) => setName(e.target.value)}
        />
      </div>

      <div className="space-y-1.5">
        <label htmlFor="task-instructions" className={fieldLabelClass()}>
          Instructions
        </label>
        <Textarea
          id="task-instructions"
          value={instructions}
          placeholder="What should the agent do each run?"
          rows={3}
          onChange={(e) => setInstructions(e.target.value)}
        />
      </div>

      <div className="space-y-1.5">
        <label htmlFor="task-workspace" className={fieldLabelClass()}>
          Workspace
        </label>
        <div className="flex items-center gap-1.5">
          <Input
            id="task-workspace"
            value={workspace}
            placeholder="Default workspace"
            autoComplete="off"
            spellCheck={false}
            className="font-mono text-[12px]"
            onChange={(e) => setWorkspace(e.target.value)}
          />
          {workspace ? (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="shrink-0 text-muted-foreground hover:text-destructive"
              onClick={() => setWorkspace("")}
            >
              Clear
            </Button>
          ) : null}
          {IN_TAURI ? (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="shrink-0"
              onClick={() => void onBrowse()}
            >
              <Folder /> Browse
            </Button>
          ) : null}
        </div>
        <p className="text-[11px] text-muted-foreground">
          The folder each run works in. Leave blank to inherit the default
          workspace.
        </p>
      </div>

      <div className="space-y-1.5">
        <span className={fieldLabelClass()}>Profile</span>
        <Select value={profile} onValueChange={setProfile}>
          <SelectTrigger aria-label="Profile">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={INHERIT}>Inherit active profile</SelectItem>
            {profiles.map((p) => (
              <SelectItem key={p.id} value={p.id}>
                {p.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <ScheduleBuilder
        value={schedule}
        onChange={setSchedule}
        preview={preview}
      />

      <SettingsSwitch
        label="Allow file changes"
        description="Lets a run write to disk. Off keeps runs read-only."
        checked={allowWrite}
        onCheckedChange={setAllowWrite}
      />

      <div className="flex items-center justify-end gap-1.5 pt-0.5">
        <Button type="button" variant="ghost" size="sm" onClick={onDone}>
          Cancel
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canSubmit}
          onClick={() => void submit()}
        >
          {isEdit ? "Save" : "Create"}
        </Button>
      </div>
    </div>
  );
}
