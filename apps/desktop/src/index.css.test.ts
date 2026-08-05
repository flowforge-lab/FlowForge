// Guards the read side of the runtime font token (#1196).
//
// `applyFont` set `--font-sans` correctly for months, with `lib/fonts.test.ts` green the
// whole time, while the font picker did nothing: no compiled selector read the variable.
// There were TWO independent breaks, and either alone is enough to make the picker look
// dead, so both are pinned here:
//
//  1. Tailwind v4's `@theme inline` compiles the `font-sans` utility to the literal value
//     instead of `var(--font-sans)`, so `html { @apply font-sans }` and every `font-sans`
//     class were hardcoded to Geist.
//  2. The boot style in `index.html` is UNLAYERED, so its `font: 14px/1.4 system-ui`
//     outranked `@layer base { html { @apply font-sans } }` regardless of what that
//     compiled to. Measured in a browser: body text was `system-ui` the whole time — it
//     had never been Geist either — and stayed `system-ui` through every font change.
//
// Writing a token nothing reads is the failure mode here, so these tests assert the
// source shape that keeps it readable.
//
// Deliberately not a computed-style test: jsdom does not substitute `var()`, so
// `getComputedStyle(document.documentElement).fontFamily` returns the unresolved string
// and would pass under the bug too. The compiled-output half is verified by grepping the
// build for `var(--font-sans)` (0 occurrences before the fix, 3 after) — recorded on the
// PR, since asserting it here would mean running a full production build per test run.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

/** Drop `/* … *\/` comments so assertions read declarations, not the prose about them —
 *  these tests are commented in terms of the very tokens they check. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, "");
}

const css = stripComments(
  readFileSync(new URL("./index.css", import.meta.url), "utf8"),
);

/** Body of the `@theme` block opened by `header`. Both blocks hold flat declarations
 *  only, so the first `}` after the header closes them. */
function themeBlock(header: "@theme {" | "@theme inline {"): string {
  const start = css.indexOf(header);
  expect(start, `${header} must exist in index.css`).toBeGreaterThan(-1);
  return css.slice(start + header.length, css.indexOf("}", start));
}

describe("font tokens in index.css", () => {
  it("keeps --font-sans out of @theme inline", () => {
    // The regression pin: move this token back under `inline` and the picker silently
    // stops working again, with every other font test still passing.
    expect(themeBlock("@theme inline {")).not.toContain("--font-sans");
  });

  it("declares --font-sans in a plain @theme so utilities emit var()", () => {
    expect(themeBlock("@theme {")).toContain("--font-sans:");
  });

  it("keeps --font-heading with --font-sans, since its value is var(--font-sans)", () => {
    // Left behind under `inline` it resolved at build time and, having no consumer, was
    // dropped from the output entirely. Here it resolves at runtime the moment anything
    // starts using `font-heading`.
    expect(themeBlock("@theme {")).toContain(
      "--font-heading: var(--font-sans)",
    );
  });

  it("leaves --font-code inline, where a direct var() read still resolves", () => {
    // Not user-configurable, and `inline` never stopped a hand-written `var(--font-code)`
    // from resolving — the keyword only changes how utilities compile. Pinned so the
    // split across the two blocks reads as deliberate rather than as an oversight.
    expect(themeBlock("@theme inline {")).toContain("--font-code:");
  });
});

describe("boot style in index.html", () => {
  const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  const bootStyle = stripComments(
    html.slice(html.indexOf("<style>"), html.indexOf("</style>")),
  );

  it("gives html/body the font token, not a literal family", () => {
    // This rule sits outside any cascade layer, so it beats everything in
    // `@layer base` — including the `html { @apply font-sans }` that the rest of this
    // file exists to protect. A literal family here silently overrides the user's
    // choice for all body text, which is how half of #1196 survived the obvious fix.
    expect(bootStyle).toContain("font-family: var(--font-sans");
  });

  it("does not use the `font:` shorthand, which resets font-family", () => {
    // `font: 14px/1.4 system-ui` was the original form: the shorthand sets family as
    // well as size, so a size-only intent silently pinned the family too.
    expect(bootStyle).not.toMatch(/\bfont:\s/);
  });
});
