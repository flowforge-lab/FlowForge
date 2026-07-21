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
              onClick={() => void install(activeUpdateChannel())}
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
            {installing ? (
              // Real download progress (#566): determinate when a content length is
              // known, indeterminate (pulsing) track otherwise.
              <div className="border-b px-3 py-2.5 last:border-b-0">
                <Progress
                  value={percent}
                  className={percent == null ? "animate-pulse" : undefined}
                />
              </div>
            ) : null}
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

      {toast ? (
        <p
          role="status"
          aria-live="polite"
          className="rounded-md border bg-muted/40 px-3 py-2 text-[12px] text-foreground"
        >
          {toast}
        </p>
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
