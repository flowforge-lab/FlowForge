import { useState } from "react";
import { Check, ChevronRight, Plus, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  MODE_CELLS,
  MODE_COLUMNS,
  PERMISSION_ROWS,
  type CellMark,
  type ControlOverrides,
} from "@/lib/control";
import { useControlConfigStore } from "@/store/control-config";

function CellMarkIcon({ mark }: { mark: CellMark }) {
  if (mark === "check") {
    return <Check className="size-3.5 text-foreground" aria-label="Allowed" />;
  }
  if (mark === "cross") {
    return (
      <X className="size-3.5 text-muted-foreground/40" aria-label="Blocked" />
    );
  }
  return <span className="text-[11px] text-muted-foreground">Ask</span>;
}

/** The Default Mode matrix: selectable columns (modes) × permission rows. */
function ModeMatrix({
  defaultMode,
  onSelectMode,
  disabled,
}: {
  defaultMode: string;
  onSelectMode: (mode: (typeof MODE_COLUMNS)[number]["value"]) => void;
  disabled: boolean;
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-border">
      <table className="w-full border-collapse text-[12px]">
        <thead>
          <tr>
            <th className="w-2/5 px-3 py-2" />
            {MODE_COLUMNS.map((col) => {
              const selected = col.value === defaultMode;
              return (
                <th key={col.value} className="p-1.5">
                  <button
                    type="button"
                    aria-pressed={selected}
                    disabled={disabled}
                    onClick={() => onSelectMode(col.value)}
                    className={cn(
                      "flex w-full flex-col items-center gap-0.5 rounded-md border px-2 py-1.5 transition-colors",
                      selected
                        ? "border-primary bg-primary/5 ring-2 ring-primary/30"
                        : "border-border bg-card hover:bg-muted/50",
                      disabled && "cursor-not-allowed opacity-50",
                    )}
                  >
                    <span className="text-[12px] font-medium text-foreground">
                      {col.label}
                    </span>
                    <span className="text-[10px] text-muted-foreground">
                      {col.sublabel}
                    </span>
                  </button>
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {PERMISSION_ROWS.map((row) => (
            <tr key={row.key} className="border-t border-border">
              <td className="px-3 py-2 text-foreground">{row.label}</td>
              {MODE_COLUMNS.map((col) => (
                <td
                  key={col.value}
                  className={cn(
                    "py-2 text-center align-middle",
                    col.value === defaultMode && "bg-primary/5",
                  )}
                >
                  <span className="inline-flex justify-center">
                    <CellMarkIcon mark={MODE_CELLS[col.value][row.key]} />
                  </span>
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const OVERRIDE_META: ReadonlyArray<{
  key: keyof ControlOverrides;
  label: string;
  placeholder: string;
}> = [
  { key: "denied", label: "Denied", placeholder: "tool or pattern to deny" },
  {
    key: "requireApproval",
    label: "Require approval",
    placeholder: "tool or pattern to gate",
  },
  { key: "allowed", label: "Allowed", placeholder: "tool or pattern to allow" },
];

/** One collapsible override bucket with count, add input, and removable rows. */
function OverrideBucket({
  label,
  placeholder,
  items,
  disabled,
  onAdd,
  onRemove,
}: {
  label: string;
  placeholder: string;
  items: string[];
  disabled: boolean;
  onAdd: (value: string) => void;
  onRemove: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");

  const submit = () => {
    if (value.trim() === "") return;
    onAdd(value);
    setValue("");
  };

  return (
    <div className="rounded-lg border border-border">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-3 py-2 text-left"
      >
        <ChevronRight
          className={cn(
            "size-3.5 text-muted-foreground transition-transform",
            open && "rotate-90",
          )}
        />
        <span className="text-[12px] font-medium text-foreground">{label}</span>
        <span className="ml-auto text-[11px] text-muted-foreground">
          {items.length}
        </span>
      </button>

      {open ? (
        <div className="space-y-2 border-t border-border px-3 py-2.5">
          <div className="flex items-center gap-1.5">
            <Input
              value={value}
              placeholder={placeholder}
              autoComplete="off"
              disabled={disabled}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submit();
                }
              }}
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={disabled || value.trim() === ""}
              onClick={submit}
            >
              <Plus />
              Add
            </Button>
          </div>

          {items.length === 0 ? (
            <p className="text-[11px] text-muted-foreground">
              Nothing here yet.
            </p>
          ) : (
            <ul className="flex flex-col gap-1">
              {items.map((item) => (
                <li
                  key={item}
                  className="flex items-center gap-2 rounded-md bg-muted/50 px-2 py-1"
                >
                  <code className="min-w-0 flex-1 truncate text-[11px] text-foreground">
                    {item}
                  </code>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="text-muted-foreground hover:text-destructive"
                    disabled={disabled}
                    onClick={() => onRemove(item)}
                    title={`Remove ${item}`}
                  >
                    <X />
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </div>
  );
}

/** Permissions sub-tab: the Default Mode matrix + Custom Overrides. */
export function PermissionsTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const setDefaultMode = useControlConfigStore((s) => s.setDefaultMode);
  const addOverride = useControlConfigStore((s) => s.addOverride);
  const removeOverride = useControlConfigStore((s) => s.removeOverride);

  if (!config) return null;

  return (
    <div className="space-y-5">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">
          Default mode
        </h3>
        <p className="text-[12px] leading-relaxed text-muted-foreground">
          Choose how much the agent can do without asking. (Presentation only
          for now — does not yet drive runtime approval.)
        </p>
        <ModeMatrix
          defaultMode={config.defaultMode}
          onSelectMode={(mode) => void setDefaultMode(mode)}
          disabled={saving}
        />
      </section>

      <section className="space-y-2 border-t pt-5">
        <h3 className="text-[13px] font-medium text-foreground">
          Custom overrides
        </h3>
        <div className="space-y-2">
          {OVERRIDE_META.map((meta) => (
            <OverrideBucket
              key={meta.key}
              label={meta.label}
              placeholder={meta.placeholder}
              items={config.overrides[meta.key]}
              disabled={saving}
              onAdd={(value) => void addOverride(meta.key, value)}
              onRemove={(value) => void removeOverride(meta.key, value)}
            />
          ))}
        </div>
      </section>
    </div>
  );
}
