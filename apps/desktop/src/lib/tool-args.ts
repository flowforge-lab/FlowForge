// Pretty-print tool-call arguments for the tool-step view. Long string values
// (a `write` tool's `content`, an `edit`'s `old_str`/`new_str`) can be entire
// files, so they are truncated to keep the step block readable — the full
// output remains available via the tool result, not the args echo.

const MAX_STRING_LEN = 300;

function truncateStrings(value: unknown): unknown {
  if (typeof value === "string") {
    return value.length > MAX_STRING_LEN
      ? `${value.slice(0, MAX_STRING_LEN)}… (${value.length} chars)`
      : value;
  }
  if (Array.isArray(value)) {
    return value.map(truncateStrings);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [
        k,
        truncateStrings(v),
      ]),
    );
  }
  return value;
}

export function formatArgs(args: unknown): string {
  try {
    return JSON.stringify(truncateStrings(args), null, 2);
  } catch {
    return String(args);
  }
}
