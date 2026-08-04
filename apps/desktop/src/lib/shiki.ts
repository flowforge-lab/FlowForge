/**
 * Shiki syntax highlighting (#1169) — sole owner of the highlighter instance,
 * the grammar loading policy, and the result cache.
 *
 * Shiki tokenizes with real TextMate grammars rather than highlight.js's
 * heuristics, which is the whole point of the swap, but the grammars are also
 * far bigger — so they are loaded lazily, one language at a time, only when a
 * block of that language actually appears. That makes highlighting async:
 * callers render plain text first and upgrade in place once the promise
 * settles (see `ShikiCode` in components/markdown.tsx).
 *
 * Colours are emitted as `var(--shiki-token-*)` (Shiki's CSS-variables theme)
 * rather than baked-in hex, so light/dark stays a pure `.dark` cascade in
 * index.css with no JS theme subscription and no re-highlight on theme change.
 */
import type { Root } from "hast";
import type { BundledLanguage, SpecialLanguage } from "shiki";

// Languages Shiki accepts without loading a grammar (it renders them unstyled).
// Anything we can't resolve falls back to `PLAIN`.
const PLAIN_LANGS = new Set<string>(["text", "plaintext", "txt", "ansi"]);
const PLAIN: SpecialLanguage = "text";

const THEME = "flowforge-css-vars";

// Bounded LRU: the transcript is virtualized (#1143), so a code block unmounts
// and remounts as the user scrolls past it. Without a cache each remount would
// flash plain text before re-resolving; with one it paints highlighted on the
// very first frame. Bounded so a long session can't grow it without limit.
const MAX_CACHE = 256;
const cache = new Map<string, Root>();

function cacheKey(code: string, lang: string): string {
  return `${lang}\0${code}`;
}

function remember(key: string, hast: Root): Root {
  cache.delete(key);
  cache.set(key, hast);
  if (cache.size > MAX_CACHE) {
    const oldest = cache.keys().next();
    if (!oldest.done) cache.delete(oldest.value);
  }
  return hast;
}

/** Synchronous cache peek, so a cached block renders highlighted immediately. */
export function getCachedHighlight(code: string, lang: string): Root | null {
  const key = cacheKey(code, lang);
  const hit = cache.get(key);
  if (!hit) return null;
  // Refresh recency so blocks the user keeps scrolling past stay resident.
  return remember(key, hit);
}

type Shiki = typeof import("shiki");
type Highlighter = Awaited<ReturnType<Shiki["createHighlighter"]>>;

let shikiPromise: Promise<{ mod: Shiki; highlighter: Highlighter }> | null =
  null;

// Dynamic import so Shiki and its oniguruma WASM land in their own chunk
// instead of the entry bundle — nothing is fetched until the first code block.
function getShiki(): Promise<{ mod: Shiki; highlighter: Highlighter }> {
  shikiPromise ??= (async () => {
    const mod = await import("shiki");
    const highlighter = await mod.createHighlighter({
      themes: [
        mod.createCssVariablesTheme({
          name: THEME,
          variablePrefix: "--shiki-",
          fontStyle: true,
        }),
      ],
      langs: [], // loaded on demand by `highlight`
    });
    return { mod, highlighter };
  })();
  return shikiPromise;
}

/** Model-supplied language ids are arbitrary; unknown ones degrade to plain. */
function resolveLang(
  mod: Shiki,
  lang: string,
): BundledLanguage | SpecialLanguage {
  const id = lang.trim().toLowerCase();
  if (!id) return PLAIN;
  if (PLAIN_LANGS.has(id)) return id as SpecialLanguage;
  return id in mod.bundledLanguages ? (id as BundledLanguage) : PLAIN;
}

/**
 * Highlights `code`, resolving to the bare `<code>` HAST (Shiki's `<pre>`
 * wrapper is dropped — callers own their own `<pre>` and its chrome).
 * Never rejects: a grammar that fails to load degrades to plain text.
 */
export async function highlight(code: string, lang: string): Promise<Root> {
  const key = cacheKey(code, lang);
  const hit = cache.get(key);
  if (hit) return remember(key, hit);

  const { mod, highlighter } = await getShiki();
  let resolved = resolveLang(mod, lang);
  if (!PLAIN_LANGS.has(resolved)) {
    try {
      await highlighter.loadLanguage(resolved);
    } catch {
      resolved = PLAIN;
    }
  }

  // `codeToHast` is the last thing that can throw, and the callers cannot
  // handle it: `ShikiCodeInner` does `void highlight(...).then(...)`, so a
  // rejection here would surface as an unhandled rejection rather than as
  // anything the user could act on. Fall back to the unstyled tree so the
  // docstring's "never rejects" is actually true.
  try {
    const root = highlighter.codeToHast(code, { lang: resolved, theme: THEME });
    return remember(key, unwrapPre(root));
  } catch (err) {
    console.error(
      `[shiki] highlighting "${lang}" failed; rendering plain`,
      err,
    );
    const root = highlighter.codeToHast(code, { lang: PLAIN, theme: THEME });
    return remember(key, unwrapPre(root));
  }
}

// Shiki emits `<pre class="shiki …"><code>…</code></pre>`. We keep only the
// `<code>`, tagged `.shiki` so index.css can style it, because CodeBlock and
// the split panel each supply their own scrolling `<pre>`.
function unwrapPre(root: Root): Root {
  const pre = root.children[0];
  if (pre?.type !== "element" || pre.tagName !== "pre") return root;
  const code = pre.children.find(
    (child) => child.type === "element" && child.tagName === "code",
  );
  if (!code || code.type !== "element") return root;

  return {
    type: "root",
    children: [{ ...code, properties: { ...code.properties, class: "shiki" } }],
  };
}
