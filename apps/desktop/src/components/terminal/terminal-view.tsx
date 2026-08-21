import { useEffect, useRef } from "react";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { ipc } from "@/lib/ipc";
import { useTerminalStore } from "@/store/terminal";

// One live shell inside the drawer (#1284): an xterm instance bound to one
// backend PTY.
//
// The component owns the xterm object outright — it is imperative, not
// React-rendered, so it lives in a ref and is created once per tab. Every tab in
// a pane stays mounted, visible or not: unmounting would dispose the terminal
// and kill its shell, and a background tab whose `npm run dev` died on a tab
// switch is not a terminal.
//
// xterm itself is imported *dynamically*, for two reasons: it is ~300KB that
// only matters once someone opens a drawer, and its module initializer touches
// a canvas 2D context on load — which a static import would run in every
// jsdom-rendered pane test, not just the ones about terminals.

export function TerminalView({
  sessionId,
  tabId,
  terminalId,
  visible,
}: {
  sessionId: string;
  tabId: string;
  terminalId: string | null;
  visible: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  // The id this view opened, read by cleanup and resize without re-running the
  // mount effect (which would open a second shell).
  const idRef = useRef<string | null>(null);

  // Open exactly one shell per tab. Keyed on `tabId`, never on `terminalId` or
  // `visible`: this effect is what *creates* the terminal id, so depending on it
  // would loop, and re-running on a tab switch would strand the old shell.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    let disposed = false;
    // Filled in once the dynamic import lands; the cleanup below closes over
    // them, so it works whether or not the chunk arrived first.
    let term: Terminal | null = null;
    let onData: { dispose: () => void } | null = null;
    let observer: ResizeObserver | null = null;

    void (async () => {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("@xterm/xterm"),
        import("@xterm/addon-fit"),
      ]);
      if (disposed) return; // unmounted while the chunk was loading

      const xterm = new Terminal({
        cursorBlink: true,
        fontFamily: cssValue("--font-code", "monospace"),
        fontSize: 12,
        // Deep enough that a build log stays scrollable, cheap enough that a
        // dozen idle tabs don't hold megabytes of text.
        scrollback: 5000,
        theme: xtermTheme(),
      });
      const fit = new FitAddon();
      xterm.loadAddon(fit);
      xterm.open(host);
      term = xterm;
      termRef.current = xterm;
      fitRef.current = fit;

      // Measure before opening the shell so its very first prompt is already
      // wrapped to the real width. `fit()` throws when the element has no
      // layout yet, which is not an error worth surfacing — the observer below
      // fits again as soon as it does.
      safeFit(fit);

      onData = xterm.onData((data) => {
        const id = idRef.current;
        if (id) void ipc.writeTerminal(id, data);
      });

      // Reflow on any size change — the drawer being dragged, the window
      // resizing, a split being added. `fit()` recomputes cols/rows from the
      // element; the shell only needs telling when they actually changed.
      let lastCols = xterm.cols;
      let lastRows = xterm.rows;
      observer = new ResizeObserver(() => {
        if (!safeFit(fit)) return;
        const id = idRef.current;
        if (!id || (xterm.cols === lastCols && xterm.rows === lastRows)) return;
        lastCols = xterm.cols;
        lastRows = xterm.rows;
        void ipc.resizeTerminal(id, xterm.cols, xterm.rows);
      });
      observer.observe(host);

      try {
        const id = await ipc.openTerminal(
          sessionId,
          xterm.cols,
          xterm.rows,
          (bytes) => {
            // Raw PTY bytes: hand them to xterm undecoded. A multi-byte
            // character can straddle two chunks, and xterm's decoder is what
            // stitches those back together — decoding here would corrupt it.
            xterm.write(bytes);
          },
        );
        if (disposed) {
          // The tab closed while the shell was still opening; nothing is bound
          // to this id, so kill it rather than leaking a shell.
          void ipc.closeTerminal(id);
          return;
        }
        idRef.current = id;
        useTerminalStore.getState().bindTerminal(sessionId, tabId, id);
        xterm.focus();
      } catch (e) {
        xterm.write(
          `\r\n\x1b[31mcould not start a shell: ${errMsg(e)}\x1b[0m\r\n`,
        );
      }
    })();

    return () => {
      disposed = true;
      observer?.disconnect();
      onData?.dispose();
      term?.dispose();
      termRef.current = null;
      fitRef.current = null;
      // Kill the shell this view owns. `closeTab` already does so for a tab the
      // user closed; this covers the paths it cannot see — the pane closing, the
      // session being swapped out, the app tearing down.
      const id = idRef.current;
      if (id) void ipc.closeTerminal(id);
      idRef.current = null;
    };
  }, [sessionId, tabId]);

  // Re-theme in place when the window's theme changes, so a terminal that has
  // been open for hours is never the one dark rectangle in a light window.
  //
  // Watches the `.dark` class on `<html>` rather than the stored preference:
  // the preference is "system" for most users and does not change when the OS
  // flips appearance — the class is what actually changes in every case
  // (preference switch, OS switch, accent theme), and it is the same signal the
  // CSS tokens themselves key on.
  useEffect(() => {
    const observer = new MutationObserver(() => {
      const term = termRef.current;
      if (term) term.options.theme = xtermTheme();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    return () => observer.disconnect();
  }, []);

  // A tab becoming visible went from `display: none` to laid out, so its last
  // fit was measured against nothing. Re-fit and hand focus to the shell, which
  // is what the user just asked for by clicking the tab.
  useEffect(() => {
    if (!visible) return;
    const fit = fitRef.current;
    if (fit) safeFit(fit);
    termRef.current?.focus();
  }, [visible, terminalId]);

  return (
    <div
      ref={hostRef}
      data-testid="terminal-host"
      // `hidden` rather than unmounted: an inactive tab's shell must keep
      // running (see the note at the top of this file).
      className={visible ? "h-full w-full" : "hidden"}
    />
  );
}

/** `fit()` throws when the host element has no layout — hidden tab, drawer still
 *  animating in, component detached. That is a normal transient state, not an
 *  error: report it as "not fitted" and let the next resize try again. */
function safeFit(fit: FitAddon): boolean {
  try {
    fit.fit();
    return true;
  } catch {
    return false;
  }
}

/** Resolve a CSS custom property to a concrete value, for the imperative xterm
 *  options that cannot take `var(...)`. */
function cssValue(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return value || fallback;
}

/** Resolve a CSS color token to a hex color xterm can parse.
 *
 *  The app's palette is authored in `oklch()`, which xterm's color parser does
 *  not understand — handed one, it falls back to black, which is how a light
 *  theme ends up with a black terminal. Assigning to a canvas `fillStyle` is not
 *  enough either: browsers now normalize `oklch()` to `oklch()`. Painting one
 *  pixel and reading it back is, because `getImageData` is always plain RGBA.
 *
 *  `fillStyle` is seeded with `fallback` first: an unparseable value leaves the
 *  previous one in place, so the pixel we read is the fallback rather than the
 *  spec's default black.
 */
function cssColor(name: string, fallback: string): string {
  const raw = cssValue(name, "");
  if (!raw) return fallback;
  try {
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) return fallback;
    ctx.fillStyle = fallback;
    ctx.fillStyle = raw;
    ctx.fillRect(0, 0, 1, 1);
    const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
    return `#${hex(r)}${hex(g)}${hex(b)}`;
  } catch {
    // No canvas (jsdom) or a tainted context: the token stays unresolved and the
    // caller's fallback keeps the terminal readable.
    return fallback;
  }
}

function hex(channel: number): string {
  return channel.toString(16).padStart(2, "0");
}

/** xterm's theme, derived from the app's own tokens so light/dark and any future
 *  palette change carry over.
 *
 *  The background is the drawer's own `--card`, not a transparent color: xterm
 *  paints `.xterm-viewport` from `theme.background`, and a zero-alpha value ends
 *  up as opaque black there (verified in the running app) — a black rectangle in
 *  a light window. Matching the surface explicitly is what actually makes the
 *  terminal look like part of the pane. */
function xtermTheme(): Record<string, string> {
  const foreground = cssColor("--foreground", "#d4d4d4");
  return {
    background: cssColor("--card", "#1e1e1e"),
    foreground,
    cursor: cssColor("--primary", foreground),
    cursorAccent: cssColor("--background", "#000000"),
    selectionBackground: cssColor("--accent", "#3a3d41"),
  };
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
