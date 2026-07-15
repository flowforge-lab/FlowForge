import { useEffect, useRef, useState } from "react";

// Clipboard write + transient "Copied" flash (1500ms), shared by every copy
// affordance (message-actions, markdown code blocks, the context-usage popover).
// Fail-quiet — the clipboard can be unavailable in an insecure context or without
// permission, in which case we simply skip the flash rather than throw.
export function useCopied() {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable (permissions / insecure context); fail quiet.
    }
  };
  return { copied, copy };
}
