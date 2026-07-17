import { SettingsSwitch } from "@/components/settings/switch";
import { usePrefsStore } from "@/store/prefs";

/**
 * Notifications sub-tab: a master switch gating the child toggles. Drives the
 * background-session toasts, sound cue, and title flash (#994). Errors always
 * surface under the master switch — they aren't gated by "Message complete".
 */
export function NotificationsTab() {
  const notifications = usePrefsStore((s) => s.notifications);
  const setNotifications = usePrefsStore((s) => s.setNotifications);
  const childrenDisabled = !notifications.enabled;

  return (
    <div className="space-y-5">
      <SettingsSwitch
        label="Notifications"
        description="Master switch for toasts, sound, and the title flash when the window is in the background. Errors always notify while this is on."
        checked={notifications.enabled}
        onCheckedChange={(enabled) => setNotifications({ enabled })}
      />

      <div className="space-y-4 border-t pt-5">
        <SettingsSwitch
          label="Message complete"
          description="Notify when a background turn finishes or stops without an answer."
          checked={notifications.messageComplete}
          disabled={childrenDisabled}
          onCheckedChange={(messageComplete) =>
            setNotifications({ messageComplete })
          }
        />
        <SettingsSwitch
          label="Approval requests"
          description="Notify when a background turn needs your approval or an answer."
          checked={notifications.approvalRequests}
          disabled={childrenDisabled}
          onCheckedChange={(approvalRequests) =>
            setNotifications({ approvalRequests })
          }
        />
        <SettingsSwitch
          label="Sound"
          description="Play a short chime with each notification."
          checked={notifications.sound}
          disabled={childrenDisabled}
          onCheckedChange={(sound) => setNotifications({ sound })}
        />
      </div>
    </div>
  );
}
