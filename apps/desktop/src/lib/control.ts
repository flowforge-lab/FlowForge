// Control settings — types + presentation metadata (#127). FE-owned for now:
// there is no backend/ts-rs type yet, and the permission matrix does NOT map to
// `ApprovalSafety` ("write"|"dangerous"). This is presentation + mock storage
// only; it does not drive runtime approval. See the TODO in lib/ipc.ts.
//
// Kept free of React/stores so the defaults + matrix derivation are unit-testable
// in vitest's node env (mirrors lib/search.ts / lib/mcp.ts).

import type {
  Safety,
  PermissionCell,
  PermissionOverrideEntry,
} from "@/bindings";

export type DefaultMode = "plan" | "auto" | "act";

export type PermissionRow =
  | "read"
  | "localWrites"
  | "externalChanges"
  | "dangerous";

/** A teammate profile (SET.12). FE-only mock until real teammate spawning lands. */
export interface Teammate {
  id: string;
  name: string;
  /** Short handle, e.g. `reviewer`. */
  slug: string;
  description: string;
}

/** Per-profile UI customization (SET.12). All FE-only presentation for now. */
export interface ControlUi {
  /** Accent color as a hex string; `""` means use the theme default. */
  accentColor: string;
  /** Stub paths until real file dialogs land (logo / favicon). */
  logoPath: string;
  faviconPath: string;
  /** Show a contextual greeting on the empty session screen. */
  contextualGreeting: boolean;
}

/** The full control config, round-tripped via `getControlConfig`/`setControlConfig`.
 *  NOTE: the global default mode is NOT here — it lives in the backend `mode.json`
 *  (via `getDefaultMode`/`setDefaultMode`, surfaced through `usePrefsStore`, #798). */
export interface ControlConfig {
  injectMemory: boolean;
  /** Backed by `user_instructions.md` once the backend lands. */
  userInstructions: string;
  promptFiles: string[];
  /** Teammate profiles (SET.12). */
  teammates: Teammate[];
  /** Per-profile UI customization (SET.12). */
  ui: ControlUi;
}

/** How a cell renders in the matrix. */
export type CellMark = "check" | "cross" | "ask";

export interface ModeColumn {
  value: DefaultMode;
  label: string;
  sublabel: string;
}

/** Matrix columns, left → right (increasing capability). */
export const MODE_COLUMNS: ReadonlyArray<ModeColumn> = [
  { value: "plan", label: "Plan", sublabel: "Read Only" },
  { value: "auto", label: "Auto", sublabel: "Balanced" },
  { value: "act", label: "Act", sublabel: "Full Access" },
];

export interface PermissionRowMeta {
  key: PermissionRow;
  label: string;
}

/** Matrix rows, top → bottom (increasing risk). */
export const PERMISSION_ROWS: ReadonlyArray<PermissionRowMeta> = [
  { key: "read", label: "Read & browse" },
  { key: "localWrites", label: "Local writes" },
  { key: "externalChanges", label: "External changes" },
  { key: "dangerous", label: "Dangerous commands" },
];

// ─── Live permission-matrix mapping (#702) ──────────────────────────────────
// Bridges the FE presentation vocabulary (rows/marks) to the real backend matrix
// (`Safety`/`PermissionCell` from bindings). The matrix grid drives runtime approval.

/** Presentation row → backend `Safety` tier. `DefaultMode` doubles as `Mode`
 *  (identical string members), so no mode mapping is needed. */
export const ROW_SAFETY: Record<PermissionRow, Safety> = {
  read: "readonly",
  localWrites: "write",
  externalChanges: "sensitive",
  dangerous: "dangerous",
};

/** Render a backend cell with the existing `CellMarkIcon` vocabulary. */
export function cellToMark(cell: PermissionCell): CellMark {
  if (cell === "allow") return "check";
  if (cell === "deny") return "cross";
  return "ask";
}

/** Next value when a matrix cell is clicked: Allow → Ask → Deny → Allow. */
export function cycleCell(cell: PermissionCell): PermissionCell {
  if (cell === "allow") return "ask";
  if (cell === "ask") return "deny";
  return "allow";
}

/** Human-readable label for a cell state (tooltips / aria). */
export function cellLabel(cell: PermissionCell): string {
  if (cell === "allow") return "Allowed";
  if (cell === "ask") return "Ask first";
  return "Denied";
}

// ─── Per-tool override buckets (#700/#702) ──────────────────────────────────
// The Custom Overrides UI groups the flat `PermissionMatrixView.overrides` list
// into three buckets by cell. A tool listed here bypasses the safety matrix and
// resolves to its bucket's cell across every mode.

export interface OverrideBucketMeta {
  cell: PermissionCell;
  label: string;
  placeholder: string;
}

/** Buckets in escalating-capability order (Denied → Ask → Allowed). */
export const OVERRIDE_BUCKETS: ReadonlyArray<OverrideBucketMeta> = [
  { cell: "deny", label: "Denied", placeholder: "tool to deny" },
  { cell: "ask", label: "Require approval", placeholder: "tool to gate" },
  { cell: "allow", label: "Allowed", placeholder: "tool to allow" },
];

/** Group the flat override list into `{ allow, ask, deny }` tool-name arrays. */
export function groupOverridesByCell(
  overrides: ReadonlyArray<PermissionOverrideEntry>,
): Record<PermissionCell, string[]> {
  const grouped: Record<PermissionCell, string[]> = {
    allow: [],
    ask: [],
    deny: [],
  };
  for (const { tool, cell } of overrides) grouped[cell].push(tool);
  return grouped;
}

// ─── Mode tool-posture buckets (#801) ───────────────────────────────────────
// Since #793/#795 the matrix cell is the advertise switch: for a given mode each
// safety tier resolves to allow → auto-runs, ask → needs approval, deny → hidden
// (not advertised). Group the presentation rows by that cell so the mode pill can
// surface the current mode's posture at a glance.

/** Bucket the presentation rows by their cell for one mode's matrix row
 *  (`matrix[mode]`), in `PERMISSION_ROWS` order. Undefined row → empty buckets. */
export function bucketRowsByCell(
  row: Record<Safety, PermissionCell> | undefined,
): Record<PermissionCell, PermissionRowMeta[]> {
  const grouped: Record<PermissionCell, PermissionRowMeta[]> = {
    allow: [],
    ask: [],
    deny: [],
  };
  if (!row) return grouped;
  for (const meta of PERMISSION_ROWS)
    grouped[row[ROW_SAFETY[meta.key]]].push(meta);
  return grouped;
}

/** Normalize a free-text slug into a handle: lowercased, alphanumerics kept, every
 *  other run collapsed to a single dash, no leading/trailing dashes. Returns "" when
 *  the input has no usable characters (the slug is optional / display-only for now). */
export function slugify(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** First-run defaults — shared by the store's initial load fallback and `resetControl`. */
export const CONTROL_DEFAULTS: ControlConfig = {
  injectMemory: true,
  userInstructions: "",
  promptFiles: [],
  // Seed teammates so the Team tab is demoable offline (SET.12); reset restores them.
  teammates: [
    {
      id: "reviewer",
      name: "Riley Reviewer",
      slug: "reviewer",
      description: "Scans diffs and flags risky changes before they land.",
    },
    {
      id: "scribe",
      name: "Sam Scribe",
      slug: "scribe",
      description: "Drafts docs and changelogs from the session.",
    },
  ],
  ui: {
    accentColor: "#6366f1",
    logoPath: "",
    faviconPath: "",
    contextualGreeting: true,
  },
};
