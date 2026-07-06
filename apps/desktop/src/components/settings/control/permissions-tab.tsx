import { useEffect, useState } from "react";
import { Check, ChevronRight, Plus, X } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  MODE_COLUMNS,
  PERMISSION_ROWS,
  ROW_SAFETY,
  cellToMark,
  cycleCell,
  cellLabel,
  type CellMark,
  type ControlOverrides,
} from "@/lib/control";
import { useControlConfigStore } from "@/store/control-config";
import {
  usePermissionMatrixStore,
  type MatrixLookup,
} from "@/store/permission-matrix";
import { AlwaysApprovedTools } from "@/components/settings/control/always-approved-tools";

// Purely decorative inside the fully-labelled cell button (the button's
// aria-label already announces the state), so the marks are aria-hidden to avoid
// a double announcement.
function CellMarkIcon({ mark }: { mark: CellMark }) {
  if (mark === "check") {
    return <Check className="size-3.5 text-foreground" aria-hidden="true" />;
  }
  if (mark === "cross") {
    return (
      <X className="size-3.5 text-muted-foreground/40" aria-hidden="true" />
    );
  }
  return (
    <span className="text-[11px] text-muted-foreground" aria-hidden="true">
      Ask
    </span>
  );
}

/** The permission matrix: selectable mode columns × editable Safety-tier cells.
 *  Each cell reflects the live backend matrix and cycles Allow → Ask → Deny on
 *  click (#702). */
function ModeMatrix({
  matrix,
  defaultMode,
  onSelectMode,
  onCycleCell,
  disabled,
}: {
  matrix: MatrixLookup;
  defaultMode: string;
  onSelectMode: (mode: (typeof MODE_COLUMNS)[number]["value"]) => void;
  onCycleCell: (
    mode: (typeof MODE_COLUMNS)[number]["value"],
    row: (typeof PERMISSION_ROWS)[number]["key"],
  ) => void;
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
              {MODE_COLUMNS.map((col) => {
                const cell = matrix[col.value][ROW_SAFETY[row.key]];
                return (
                  <td
                    key={col.value}
                    className={cn(
                      "p-1 text-center align-middle",
                      col.value === defaultMode && "bg-primary/5",
                    )}
                  >
                    <button
                      type="button"
                      disabled={disabled}
                      onClick={() => onCycleCell(col.value, row.key)}
                      title={`${row.label} · ${col.label}: ${cellLabel(cell)} — click to change`}
                      aria-label={`${row.label} in ${col.label} mode: ${cellLabel(cell)}. Click to change.`}
                      className={cn(
                        "inline-flex min-w-8 items-center justify-center rounded-md px-2 py-1.5 transition-colors hover:bg-muted",
                        disabled && "cursor-not-allowed opacity-50",
                      )}
                    >
                      <CellMarkIcon mark={cellToMark(cell)} />
                    </button>
                  </td>
                );
              })}
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

/** Permissions sub-tab: the editable permission matrix + Custom Overrides. */
export function PermissionsTab() {
  const config = useControlConfigStore((s) => s.config);
  const saving = useControlConfigStore((s) => s.saving);
  const setDefaultMode = useControlConfigStore((s) => s.setDefaultMode);
  const addOverride = useControlConfigStore((s) => s.addOverride);
  const removeOverride = useControlConfigStore((s) => s.removeOverride);

  const matrix = usePermissionMatrixStore((s) => s.matrix);
  const matrixLoading = usePermissionMatrixStore((s) => s.loading);
  const matrixSaving = usePermissionMatrixStore((s) => s.saving);
  const matrixError = usePermissionMatrixStore((s) => s.error);
  const loadMatrix = usePermissionMatrixStore((s) => s.load);
  const setCell = usePermissionMatrixStore((s) => s.setCell);

  useEffect(() => {
    void loadMatrix();
  }, [loadMatrix]);

  if (!config) return null;

  return (
    <div className="space-y-5">
      <section className="space-y-2">
        <h3 className="text-[13px] font-medium text-foreground">Permissions</h3>
        <p className="text-[12px] leading-relaxed text-muted-foreground">
          What the agent may do at each safety tier, per mode. Click a cell to
          cycle Allow → Ask → Deny; changes take effect on the next tool call.
        </p>
        {matrixError && (
          <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-[12px] text-destructive">
            Couldn&apos;t update permissions: {matrixError}
          </p>
        )}
        {!matrix ? (
          <p className="px-1 py-2 text-[12px] text-muted-foreground">
            {matrixLoading ? "Loading permissions…" : "No permissions to show."}
          </p>
        ) : (
          <ModeMatrix
            matrix={matrix}
            defaultMode={config.defaultMode}
            onSelectMode={(mode) => void setDefaultMode(mode)}
            onCycleCell={(mode, row) =>
              void setCell(
                mode,
                ROW_SAFETY[row],
                cycleCell(matrix[mode][ROW_SAFETY[row]]),
              )
            }
            disabled={saving || matrixSaving}
          />
        )}
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

      <AlwaysApprovedTools />
    </div>
  );
}
