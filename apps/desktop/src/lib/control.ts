// Control settings — types + presentation metadata (#127). FE-owned for now:
// there is no backend/ts-rs type yet, and the permission matrix does NOT map to
// `ApprovalSafety` ("write"|"dangerous"). This is presentation + mock storage
// only; it does not drive runtime approval. See the TODO in lib/ipc.ts.
//
// Kept free of React/stores so the defaults + matrix derivation are unit-testable
// in vitest's node env (mirrors lib/search.ts / lib/mcp.ts).

export type DefaultMode = "plan" | "auto" | "act";

export type PermissionRow =
  | "read"
  | "localWrites"
  | "externalChanges"
  | "dangerous";

export type PermissionDecision = "allow" | "deny" | "ask";

export interface ControlOverrides {
  denied: string[];
  requireApproval: string[];
  allowed: string[];
}

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

/** The full control config, round-tripped via `getControlConfig`/`setControlConfig`. */
export interface ControlConfig {
  defaultMode: DefaultMode;
  permissionPolicy: Record<PermissionRow, PermissionDecision>;
  overrides: ControlOverrides;
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

// Canonical cell marks per mode. Capability escalates plan → auto → act; dangerous
// commands always require an explicit ask (never silently allowed).
export const MODE_CELLS: Record<
  DefaultMode,
  Record<PermissionRow, CellMark>
> = {
  plan: {
    read: "check",
    localWrites: "cross",
    externalChanges: "cross",
    dangerous: "cross",
  },
  auto: {
    read: "check",
    localWrites: "check",
    externalChanges: "ask",
    dangerous: "ask",
  },
  act: {
    read: "check",
    localWrites: "check",
    externalChanges: "check",
    dangerous: "ask",
  },
};

/** Map a presentation mark to the stored per-row decision. */
export function cellToDecision(mark: CellMark): PermissionDecision {
  if (mark === "check") return "allow";
  if (mark === "cross") return "deny";
  return "ask";
}

/** The per-row policy implied by a mode's canonical cells. */
export function policyForMode(
  mode: DefaultMode,
): Record<PermissionRow, PermissionDecision> {
  const cells = MODE_CELLS[mode];
  return {
    read: cellToDecision(cells.read),
    localWrites: cellToDecision(cells.localWrites),
    externalChanges: cellToDecision(cells.externalChanges),
    dangerous: cellToDecision(cells.dangerous),
  };
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
  defaultMode: "auto",
  permissionPolicy: policyForMode("auto"),
  overrides: { denied: [], requireApproval: [], allowed: [] },
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
