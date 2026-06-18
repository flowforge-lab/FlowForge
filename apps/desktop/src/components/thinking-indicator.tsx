// Shown in the transcript during the pending window (turn sent, no token yet)
// so the user gets immediate confirmation the model is working before any
// content streams. Assistant-aligned to read as the in-progress reply; it
// clears the moment the first token/tool-call arrives and the real row renders.
export function ThinkingIndicator() {
  return (
    <div className="flex flex-col items-start gap-1.5">
      <div
        className="flex items-center gap-1 px-0.5 py-1.5"
        role="status"
        aria-label="Thinking"
      >
        <span className="ff-thinking-dot" />
        <span className="ff-thinking-dot" />
        <span className="ff-thinking-dot" />
        <span className="sr-only">Thinking…</span>
      </div>
    </div>
  );
}
