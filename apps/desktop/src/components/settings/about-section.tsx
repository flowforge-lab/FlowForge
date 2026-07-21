import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ChevronRight, Loader2 } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import {
  ABOUT_BUG_REPORT_URL,
  ABOUT_SLACK_URL,
  APP_TAGLINE,
  formatUpdateStatus,
  getAppVersion,
  openExternalUrl,
} from "@/lib/about";
import { ipc } from "@/lib/ipc";
import { Progress } from "@/components/ui/progress";
import { Toast, ToastViewport } from "@/components/ui/toast";
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
import { useExperimentalStore } from "@/store/experimental";
import { useSettingsStore } from "@/store/settings";
import {
  progressPercent,
  activeUpdateChannel,
  useUpdateStore,
} from "@/store/update";

const TOAST_MS = 3200;

/**
 * About section (#134, SET.11): version, update/backup stubs, keyboard shortcut
 * link, and external help URLs. Version comes from Tauri metadata on mount;
 * backup/update actions are mock IPC no-ops surfaced as confirmation toasts.
 */
export function AboutSection() {
  const setSection = useSettingsStore((s) => s.setSection);
  // The "Developer" group (sidecar smoke-test) is a dev-only surface — gated
  // behind the `devTools` experimental flag so it never reaches end users.
  const devTools = useExperimentalStore((s) => s.flags.devTools);
  const [version, setVersion] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  // Downgrade confirmation (#1034) — an older local build only installs from here.
  const [confirmingDowngrade, setConfirmingDowngrade] = useState(false);

  // Update state lives in a shared store (#363) so a future background indicator
  // can reuse it; the manual check below also feeds it so "Update now" appears.
  const updateStatus = useUpdateStore((s) => s.status);
  const installing = useUpdateStore((s) => s.installing);
  const install = useUpdateStore((s) => s.install);
  const progress = useUpdateStore((s) => s.progress);
  // Determinate percent when the feed sent a content length; null -> indeterminate.
  const percent = progressPercent(progress);

  useEffect(() => {
    let alive = true;
    void getAppVersion().then((v) => alive && setVersion(v));
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (!toast) return;
    const handle = setTimeout(() => setToast(null), TOAST_MS);
    return () => clearTimeout(handle);
  }, [toast]);

  const showToast = useCallback((message: string) => setToast(message), []);

  // Runs an IPC action and toasts FE-owned copy built from its structured result.
  const runIpcAction = useCallback(
    async <T,>(action: () => Promise<T>, format: (result: T) => string) => {
      try {
        showToast(format(await action()));
      } catch (err) {
        showToast(err instanceof Error ? err.message : String(err));
      }
    },
    [showToast],
  );

  // Install the exact version this section is showing. The backend re-checks the feed
  // and refuses anything else (#1034) — the store then re-checks, so the row/dialog
  // re-prompts with the build that is really on the feed and the toast says why.
  const runInstall = useCallback(
    (expectedVersion: string, allowDowngrade = false) => {
      void install(
        activeUpdateChannel(),
        expectedVersion,
        allowDowngrade,
      ).catch((err) =>
        showToast(err instanceof Error ? err.message : String(err)),
      );
    },
    [install, showToast],
  );

  // Manual check: store the result (so the "Update now" row can appear) and toast
  // the FE-owned copy. Distinct from the store's silent background `refresh`.
  const onCheckForUpdates = useCallback(() => {
    void (async () => {
      try {
        const channel = activeUpdateChannel();
        const status = await ipc.checkForUpdates(channel);
        useUpdateStore.setState({ status });
        showToast(formatUpdateStatus(status));
      } catch (err) {
        showToast(err instanceof Error ? err.message : String(err));
      }
    })();
  }, [showToast]);

  // Real download progress (#566): determinate when a content length is known,
  // indeterminate (pulsing) track otherwise. Shared by the newer and older paths.
  const progressRow = (
    <div className="border-b px-3 py-2.5 last:border-b-0">
      <Progress
        value={percent}
        className={percent == null ? "animate-pulse" : undefined}
      />
    </div>
  );

  return (
    <div className="space-y-6">
      <p className="text-[13px] text-foreground">
        {version ? (
          <>
            Version {version} — {APP_TAGLINE}
          </>
        ) : (
          <span className="text-muted-foreground">Loading version…</span>
        )}
      </p>

      <AboutGroup>
        <AboutRow label="Check for updates" onClick={onCheckForUpdates} />
        {updateStatus?.kind === "available" ? (
          <>
            <AboutRow
              label={`Update now — version ${updateStatus.version}`}
              onClick={() => runInstall(updateStatus.version)}
              disabled={installing}
              trailing={
                installing ? (
                  <Loader2
                    className="size-4 shrink-0 animate-spin text-muted-foreground"
                    aria-hidden
                  />
                ) : undefined
              }
            />
            {installing ? progressRow : null}
          </>
        ) : null}
        {/* An older local build (#1034) is never bannered and never one-click: it
            shows here with its build identity and installs only after the user
            confirms the downgrade — the path that makes bisecting a dogfood
            regression possible. */}
        {updateStatus?.kind === "olderAvailable" ? (
          <>
            <AboutRow
              label={`Install older build — version ${updateStatus.version}`}
              onClick={() => setConfirmingDowngrade(true)}
              disabled={installing}
              trailing={
                installing ? (
                  <Loader2
                    className="size-4 shrink-0 animate-spin text-muted-foreground"
                    aria-hidden
                  />
                ) : undefined
              }
            />
            <p className="border-b px-3 py-2 text-[12px] text-muted-foreground last:border-b-0">
              Older than the running build
              {updateStatus.notes ? ` — ${updateStatus.notes}` : "."}
            </p>
            {installing ? progressRow : null}
          </>
        ) : null}
        <AboutRow
          label="What's New"
          onClick={() =>
            showToast("Release notes arrive with the in-app updater.")
          }
        />
        <AboutRow
          label="Quick Setup"
          onClick={() =>
            showToast("Quick Setup wizard arrives in a future release.")
          }
        />
      </AboutGroup>

      <AboutGroup title="Data">
        <AboutRow
          label="Export backup"
          onClick={() =>
            void runIpcAction(
              () => ipc.exportBackup(),
              (r) => `Backup exported to ${r.path}.`,
            )
          }
        />
        <AboutRow
          label="Restore from backup"
          onClick={() =>
            void runIpcAction(
              () => ipc.restoreBackup(),
              (r) => `Backup restored from ${r.path}.`,
            )
          }
        />
      </AboutGroup>

      {devTools ? (
        <AboutGroup title="Developer">
          <AboutRow
            label="Run sidecar smoke-test"
            onClick={() =>
              void runIpcAction(
                () => ipc.runSidecarTurn("hello"),
                (r) =>
                  `Sidecar turn completed: ${r.events} event(s) on session ${r.session_id.slice(0, 8)}.`,
              )
            }
          />
        </AboutGroup>
      ) : null}

      <button
        type="button"
        className="flex w-full items-center gap-1 text-left text-[13px] text-primary hover:underline"
        onClick={() => setSection("keyboard")}
      >
        View all keyboard shortcuts
        <span aria-hidden>→</span>
      </button>

      <AboutGroup title="Get Help">
        <AboutRow
          label="Report a Bug"
          onClick={() => void openExternalUrl(ABOUT_BUG_REPORT_URL)}
        />
        <AboutRow
          label="Join our Slack"
          onClick={() => void openExternalUrl(ABOUT_SLACK_URL)}
        />
      </AboutGroup>

      <AlertDialog
        open={confirmingDowngrade}
        onOpenChange={setConfirmingDowngrade}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Install an older build?</AlertDialogTitle>
            <AlertDialogDescription>
              {updateStatus?.kind === "olderAvailable" ? (
                <>
                  Version {updateStatus.version} is older than the build you're
                  running. This is a downgrade — FlowForge will install it and
                  relaunch.
                  {updateStatus.notes ? ` ${updateStatus.notes}` : ""}
                </>
              ) : null}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setConfirmingDowngrade(false);
                if (updateStatus?.kind !== "olderAvailable") return;
                // The version the user just confirmed, plus the explicit downgrade
                // opt-in — the backend refuses an older build without the opt-in,
                // and refuses any *other* version than this one (#1034).
                runInstall(updateStatus.version, true);
              }}
            >
              Install older build
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Fixed viewport, not an inline block at the end of the section: About lives
          inside the settings ScrollArea, so an inline toast lands below the fold and
          is clipped — every action here (Check for updates, What's New, Quick Setup,
          the backup rows) looked dead because its only feedback was off-screen. This
          is the same anchor the app's other toasts use (#1054). */}
      {toast ? (
        <ToastViewport className="z-[60]">
          <Toast className="text-[12px]">{toast}</Toast>
        </ToastViewport>
      ) : null}
    </div>
  );
}

function AboutGroup({
  title,
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <section className="space-y-1">
      {title ? (
        <h4 className="px-0.5 pb-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground/60">
          {title}
        </h4>
      ) : null}
      <div className="overflow-hidden rounded-md border">{children}</div>
    </section>
  );
}

function AboutRow({
  label,
  onClick,
  disabled = false,
  trailing,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  /** Overrides the default chevron (e.g. a spinner while an action runs). */
  trailing?: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex w-full items-center justify-between gap-3 border-b px-3 py-2.5 text-left text-[13px] text-foreground last:border-b-0",
        "transition-colors hover:bg-muted/40",
        "disabled:pointer-events-none disabled:opacity-60",
      )}
    >
      <span>{label}</span>
      {trailing ?? (
        <ChevronRight
          className="size-4 shrink-0 text-muted-foreground"
          aria-hidden
        />
      )}
    </button>
  );
}
