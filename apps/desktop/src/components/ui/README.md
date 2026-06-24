# UI primitives (`components/ui/`)

The shared shadcn-style + Tailwind building blocks for the desktop app (#284). Feature
code composes these instead of re-rolling buttons, badges, spinners, or one-off icon
imports. Radix-backed primitives wrap `radix-ui` for behavior/ARIA and add the app's
theming on top.

## Conventions

### Icons

- Import every icon from `@/components/ui/icon` — **never** from `lucide-react` directly.
  A `no-restricted-imports` ESLint rule enforces this; `icon.ts` is the one allowed place
  to pull `lucide-react`, so it's the single chokepoint to restyle/swap the set (#284 §1).
- Add a new icon by re-exporting it from `icon.ts` (kept alphabetical).
- Default `strokeWidth` is lucide's `2`. Size and color come from `className`.

### Size scale (icons + controls share one scale)

| Token      | Use                                                           |
| ---------- | ------------------------------------------------------------- |
| `size-3`   | Compact contexts (badges, dense rows, `xs` buttons)           |
| `size-3.5` | **Default inline UI** — spinners, inline icons, `sm` controls |
| `size-4`   | Standalone / `default`-size controls                          |

`button.tsx` already maps its `size` variants to matching SVG sizes (e.g. `xs` → `size-3`,
`sm` → `size-3.5`), so icons inside buttons inherit the right scale automatically — don't
hard-code a size on an icon that lives in a `Button`.

### Theming (dark/light)

Use the semantic CSS variables, never raw colors — they flip automatically with the theme:

- Surfaces: `bg-background`, `bg-muted`, `bg-popover` (+ `text-popover-foreground`)
- Text: `text-foreground`, `text-muted-foreground`
- Intent: `text-destructive` / `bg-destructive/*`, `text-primary`, `bg-accent`
- Status tones (tinted, dark-aware): see `badge.tsx` `tone` (`neutral|amber|emerald|sky|destructive`)

Every primitive here is built on these tokens, so it themes correctly in both modes with no
per-component dark: overrides beyond what the tokens require.

## Primitives

**Controls** — `button`, `input`, `textarea`, `switch`, `select`, `tabs`, `separator`
**Overlays** — `dropdown-menu`, `context-menu`, `tooltip`, `popover`, `alert-dialog`, `toast`, `scroll-area`
**Display / status** — `badge`, `progress`, `skeleton`, `spinner`, `empty-state`, `error-state`

### State components (#284 §3)

- `Spinner` — accessible `role="status"` loading spinner (`size` `sm`/`md`). The single
  `Loader2 + animate-spin` source; color is inherited.
- `EmptyState` — optional icon + title + hint for empty lists / empty search.
- `ErrorState` — `role="alert"` message with an optional `onRetry` "Try again" button.

### Notes

- `toast.tsx` is presentational (`ToastViewport` + `Toast`); auto-dismiss / pause-on-hover
  behavior stays with the caller (see `pheno-mcp-toast.tsx`). No toast library dependency.
- `select` / `popover` are radix-backed groundwork; feature work adopts them as needed.
