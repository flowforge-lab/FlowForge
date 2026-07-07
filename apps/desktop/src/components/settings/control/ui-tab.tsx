import { ImageIcon, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { SettingsSwitch } from "@/components/settings/switch";
import { cn } from "@/lib/utils";
import { useControlConfigStore } from "@/store/control-config";

// A few brand-friendly presets next to the custom color input.
const ACCENT_PRESETS = ["#6366f1", "#10b981", "#f59e0b", "#ef4444", "#ec4899"];

/** Evaluated per call (not a module constant) so tests can toggle the branch —
 *  mirrors lib/save-file.ts. Absent under `VITE_FF_MOCK` in a plain browser tab. */
function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

/** Open a native image picker (#803). In Tauri that's the OS file dialog filtered to
 *  images; in the web/mock build (dev only — the shipped app is always Tauri) it falls
 *  back to a hand-typed path so the persist/clear flow stays exercisable. Returns the
 *  chosen path, or `null` when cancelled/empty. */
async function pickImagePath(): Promise<string | null> {
  if (inTauri()) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      multiple: false,
      filters: [
        { name: "Image", extensions: ["png", "jpg", "jpeg", "svg", "ico"] },
      ],
    });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt("Path to image file");
  const trimmed = typed?.trim();
  return trimmed ? trimmed : null;
}

/** UI sub-tab (SET.12): per-profile accent, logo/favicon pickers, greeting toggle. */
export function UiTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const setUi = useControlConfigStore((s) => s.setUi);

  if (!config) return null;
  const { ui } = config;

  return (
    <div className="space-y-5">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">
          Accent color
        </h3>
        <div className="flex items-center gap-2">
          {ACCENT_PRESETS.map((color) => (
            <button
              key={color}
              type="button"
              aria-label={`Accent ${color}`}
              aria-pressed={ui.accentColor.toLowerCase() === color}
              disabled={saving}
              onClick={() => void setUi({ accentColor: color })}
              style={{ backgroundColor: color }}
              className={cn(
                "size-6 rounded-full border outline-none transition-transform focus-visible:ring-2 focus-visible:ring-primary/30",
                ui.accentColor.toLowerCase() === color
                  ? "ring-2 ring-primary ring-offset-2 ring-offset-background"
                  : "hover:scale-110",
              )}
            />
          ))}
          <label className="ml-1 flex items-center gap-1.5 text-[11px] text-muted-foreground">
            <input
              type="color"
              value={ui.accentColor || "#6366f1"}
              disabled={saving}
              onChange={(e) => void setUi({ accentColor: e.target.value })}
              className="size-6 cursor-pointer rounded border bg-transparent"
              aria-label="Custom accent color"
            />
            <code className="text-[10px]">{ui.accentColor || "default"}</code>
          </label>
        </div>
      </section>

      <section className="space-y-3 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">Branding</h3>
        <FilePickerRow
          label="Custom logo"
          path={ui.logoPath}
          disabled={saving}
          onChoose={() =>
            void pickImagePath().then((p) => {
              if (p != null) void setUi({ logoPath: p });
            })
          }
          onClear={() => void setUi({ logoPath: "" })}
        />
        <FilePickerRow
          label="Custom favicon"
          path={ui.faviconPath}
          disabled={saving}
          onChoose={() =>
            void pickImagePath().then((p) => {
              if (p != null) void setUi({ faviconPath: p });
            })
          }
          onClear={() => void setUi({ faviconPath: "" })}
        />
      </section>

      <section className="border-t pt-5">
        <SettingsSwitch
          label="Contextual greeting"
          description="Show a personalized greeting on the empty session screen."
          checked={ui.contextualGreeting}
          disabled={saving}
          onCheckedChange={(on) => void setUi({ contextualGreeting: on })}
        />
      </section>
    </div>
  );
}

function FilePickerRow({
  label,
  path,
  disabled,
  onChoose,
  onClear,
}: {
  label: string;
  path: string;
  disabled: boolean;
  onChoose: () => void;
  onClear: () => void;
}) {
  return (
    <div className="space-y-1.5">
      <span className="text-[11px] font-medium text-muted-foreground">
        {label}
      </span>
      <div className="flex items-center gap-1.5">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={onChoose}
        >
          <ImageIcon />
          Choose file…
        </Button>
        {path ? (
          <div className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md bg-muted/50 px-2 py-1">
            <code className="min-w-0 flex-1 truncate text-[11px] text-foreground">
              {path}
            </code>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground hover:text-destructive"
              disabled={disabled}
              onClick={onClear}
              title={`Clear ${label.toLowerCase()}`}
              aria-label={`Clear ${label.toLowerCase()}`}
            >
              <X />
            </Button>
          </div>
        ) : (
          <span className="text-[11px] text-muted-foreground">No file</span>
        )}
      </div>
    </div>
  );
}
