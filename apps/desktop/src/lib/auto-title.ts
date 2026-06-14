// Pure title derivation, shared by the chat store's optimistic auto-title and the
// dev MockIpc so both agree with the backend's `auto_title` (ff-core). Kept free
// of store/React deps so it stays trivially testable.

// Words we always skip at the start of a prompt before extracting the title.
// Includes pronouns, articles, modals, common question stems, and proxy verbs
// that precede the actual subject ("understand how X" → skip to X).
const STOP = new Set([
  "a",
  "an",
  "the",
  "is",
  "are",
  "was",
  "were",
  "i",
  "you",
  "we",
  "they",
  "it",
  "he",
  "she",
  "my",
  "your",
  "our",
  "their",
  "in",
  "on",
  "at",
  "to",
  "for",
  "of",
  "and",
  "or",
  "but",
  "how",
  "what",
  "when",
  "where",
  "why",
  "who",
  "do",
  "does",
  "did",
  "can",
  "could",
  "would",
  "should",
  "will",
  "please",
  "help",
  "me",
  "us",
  // proxy verbs that prefix the real topic
  "understand",
  "explain",
  "tell",
  "show",
  "describe",
  "clarify",
  "give",
]);

/**
 * Derive a short, readable title from the user's first prompt.
 * All leading stop-words are skipped to land on the first meaningful word,
 * then word count scales with input length (2 → 5 words).
 */
export function autoTitle(content: string): string {
  const words = content.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "New session";

  // Advance past ALL leading stop-words, but always keep at least 1 word.
  let start = 0;
  while (
    start < words.length - 1 &&
    STOP.has(words[start].toLowerCase().replace(/[^a-z]/g, ""))
  ) {
    start++;
  }
  const meaningful = words.slice(start);

  // Scale word count on input length.
  const len = content.length;
  const count = Math.min(
    meaningful.length,
    len <= 25 ? 2 : len <= 50 ? 3 : len <= 100 ? 4 : 5,
  );

  const title = meaningful.slice(0, count).join(" ");
  return title.charAt(0).toUpperCase() + title.slice(1);
}
