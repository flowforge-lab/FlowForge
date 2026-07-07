/**
 * Splits streaming markdown content into "closed" blocks (won't change again
 * as more tokens arrive) and one trailing "open" block (still growing).
 *
 * While streaming, the whole message is re-parsed by remark-gfm every
 * animation frame because `content` grows every frame (#844). Splitting lets
 * the caller memoize each closed block's render — since its text is stable
 * once closed, it's parsed exactly once — and only re-parse the small open
 * tail each frame, bounding per-frame cost to roughly the last block's size
 * instead of the whole message.
 *
 * A block boundary is a blank line, but only outside an open fenced code
 * block (``` or ~~~) — otherwise a blank line inside a code sample would
 * split the fence and corrupt both halves. GFM tables have no blank lines
 * between rows, so they always stay within one block.
 */
export function splitBlocks(content: string): {
  closed: string[];
  open: string;
} {
  const lines = content.split("\n");
  const closed: string[] = [];
  let current: string[] = [];
  let fence: string | null = null; // active fence marker (e.g. "```"), or null

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fenceMatch = /^(`{3,}|~{3,})/.exec(line.trim());

    if (fence) {
      current.push(line);
      if (
        fenceMatch &&
        fenceMatch[1].startsWith(fence[0]) &&
        fenceMatch[1].length >= fence.length
      ) {
        fence = null;
      }
      continue;
    }

    if (fenceMatch) {
      current.push(line);
      fence = fenceMatch[1];
      continue;
    }

    // A blank line outside a fence ends the current block, but only if more
    // content follows — a trailing blank line with nothing after it doesn't
    // yet prove the block is closed (more text could still be appended to
    // what looks like a new blank block).
    if (line.trim() === "" && current.length > 0 && i < lines.length - 1) {
      closed.push(current.join("\n"));
      current = [];
      continue;
    }

    current.push(line);
  }

  return { closed, open: current.join("\n") };
}
