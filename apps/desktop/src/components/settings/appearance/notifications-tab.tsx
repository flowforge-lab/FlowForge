import { SettingsSwitch } from "@/components/settings/switch";
import { usePrefsStore } from "@/store/prefs";

/**
 * Notifications sub-tab: a master switch gating three child toggles. FE-only
 * flags (SET.2) — no OS notifications are fired yet.
 */
export function NotificationsTab() {
  const notifications = usePrefsStore((s) => s.notifications);
  const setNotifications = usePrefsStore((s) => s.setNotifications);
  const childrenDisabled = !notifications.enabled;

  return (
    <div className="space-y-5">
      <SettingsSwitch
        label="Notifications"
        description="Master switch for all in-app notifications."
        checked={notifications.enabled}
        onCheckedChange={(enabled) => setNotifications({ enabled })}
      />

      <div className="space-y-4 border-t pt-5">
        <SettingsSwitch
          label="Message complete"
          description="Notify when an assistant turn finishes."
          checked={notifications.messageComplete}
          disabled={childrenDisabled}
          onCheckedChange={(messageComplete) =>
            setNotifications({ messageComplete })
          }
        />
        <SettingsSwitch
          label="Approval requests"
          description="Notify when a tool call needs your approval."
          checked={notifications.approvalRequests}
          disabled={childrenDisabled}
          onCheckedChange={(approvalRequests) =>
            setNotifications({ approvalRequests })
          }
        />
        <SettingsSwitch
          label="Sound"
          description="Play a sound with notifications."
          checked={notifications.sound}
          disabled={childrenDisabled}
          onCheckedChange={(sound) => setNotifications({ sound })}
        />
      </div>
    </div>
  );
}
