import { useRef, useState } from "react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useGoalDialogStore } from "@/store/goal-dialog";
import { useGoalStore } from "@/store/goal";

// Start-goal dialog (#816, RFC 0020). The command-palette "Start goal…" entry opens
// it for the active session; on confirm it calls `goal.start`, which begins the
// autonomous loop and upserts the goal so `GoalStatusPanel` appears. Mounted at the
// app root next to the palette; the body mounts fresh each open so its fields reset
// for free (same pattern as `CommandPalette`).
export function StartGoalDialog() {
  const sessionId = useGoalDialogStore((s) => s.sessionId);
  if (sessionId === null) return null;
  return <StartGoalDialogBody sessionId={sessionId} />;
}

function StartGoalDialogBody({ sessionId }: { sessionId: string }) {
  const close = useGoalDialogStore((s) => s.close);
  const start = useGoalStore((s) => s.start);

  const [objective, setObjective] = useState("");
  // Left blank => the backend applies its own default (RFC 0020: 40), so the FE
  // never hardcodes a value that could drift from the backend default.
  const [maxIterations, setMaxIterations] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const objectiveRef = useRef<HTMLTextAreaElement>(null);

  const trimmed = objective.trim();
  const canStart = trimmed.length > 0 && !submitting;

  function parsedMaxIterations(): number | undefined {
    const raw = maxIterations.trim();
    if (raw === "") return undefined;
    const n = Number(raw);
    return Number.isInteger(n) && n > 0 ? n : undefined;
  }

  async function handleStart() {
    if (!canStart) return;
    setSubmitting(true);
    try {
      await start(sessionId, trimmed, parsedMaxIterations());
      close();
    } catch {
      // The backend rejected the goal (e.g. a loop is already running for this
      // session). Keep the dialog open so the objective isn't lost; the panel and
      // its own controls surface the existing goal's state.
      setSubmitting(false);
    }
  }

  return (
    <AlertDialog open onOpenChange={(next) => !next && close()}>
      <AlertDialogContent
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          objectiveRef.current?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>Start a goal</AlertDialogTitle>
          <AlertDialogDescription>
            The agent works autonomously toward this objective, one turn at a
            time, until it is met or the budget runs out. Pause, steer, or abort
            anytime from the panel.
          </AlertDialogDescription>
        </AlertDialogHeader>

        <div className="grid gap-3">
          <Textarea
            ref={objectiveRef}
            value={objective}
            onChange={(e) => setObjective(e.target.value)}
            onKeyDown={(e) => {
              // Cmd/Ctrl+Enter starts the goal from the textarea (keyboard-native).
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                e.preventDefault();
                void handleStart();
              }
            }}
            placeholder="e.g. Refactor the auth module and make all tests pass"
            rows={3}
            aria-label="Goal objective"
          />
          <label className="flex items-center justify-between gap-3 text-[13px] text-muted-foreground">
            <span>Max iterations</span>
            <Input
              type="number"
              min={1}
              inputMode="numeric"
              value={maxIterations}
              onChange={(e) => setMaxIterations(e.target.value)}
              placeholder="40"
              className="w-24"
              aria-label="Max iterations"
            />
          </label>
        </div>

        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="default"
            disabled={!canStart}
            onClick={(e) => {
              // Prevent Radix's auto-close so a rejected start keeps the dialog up;
              // `handleStart` closes explicitly on success.
              e.preventDefault();
              void handleStart();
            }}
          >
            Start goal
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
