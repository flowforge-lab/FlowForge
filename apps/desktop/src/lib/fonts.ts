// Font registry — swap `--font-sans` at runtime. Geist ships in index.css;
// other faces lazy-load on first selection so the default bundle stays lean.

export type Font = "geist" | "inter";

export type FontDefinition = {
  id: Font;
  label: string;
  cssValue: string;
};

// When adding a font, also update the hard-coded map in index.html `<head>`
// (zero-FOUC boot runs pre-module and cannot import this registry).
export const FONTS: FontDefinition[] = [
  {
    id: "geist",
    label: "Geist",
    cssValue: '"Geist Variable", sans-serif',
  },
  {
    id: "inter",
    label: "Inter",
    cssValue: '"Inter Variable", sans-serif',
  },
];

const loaded = new Set<Font>(["geist"]);

/** Apply a font by setting the Tailwind `--font-sans` token on `<html>`. */
export function applyFont(font: Font): void {
  const def = FONTS.find((f) => f.id === font);
  if (!def) return;
  document.documentElement.style.setProperty("--font-sans", def.cssValue);
  if (!loaded.has(font)) {
    loaded.add(font);
    if (font === "inter") {
      void import("@fontsource-variable/inter");
    }
  }
}

/** CSS value for a font id (runtime helpers/tests). */
export function fontCssValue(font: Font): string | undefined {
  return FONTS.find((f) => f.id === font)?.cssValue;
}
