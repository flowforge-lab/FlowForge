// The composer's per-model attachment capability gate (#342/#504, RFC 0005
// §11.3), lifted out of input-bar.tsx (#723) so the input bar and the pane-level
// drop overlay (session-pane.tsx) derive it identically from one place.
//
// The resolved model for a pane may accept images, documents, both, or neither.
// Caps are derived backend-side from the resolved `(kind, model)` and carried on
// `ResolvedModel` (loaded per session by the model chip), so a per-session model
// override gates the composer by the model it actually runs. Fail OPEN when
// unknown (not loaded yet / no session) so the composer is never falsely blocked.
// Vision and documents gate independently.

import type { AttachGate } from "@/lib/attachments";
import { useSessionModelStore } from "@/store/session-model";

export interface AttachGateInfo extends AttachGate {
  /** True only when neither images nor documents are accepted. */
  attachGated: boolean;
  /** Affordance copy naming only the kinds the model can take. */
  attachLabel: string;
}

export function useAttachGate(sessionId: string | undefined): AttachGateInfo {
  const resolved = useSessionModelStore((s) =>
    sessionId ? s.resolvedBySession[sessionId] : undefined,
  );
  const visionGated = resolved?.supportsVision === false;
  const docGated = resolved?.supportsDocuments === false;
  return {
    visionGated,
    docGated,
    // Fully disabled only when neither kind is allowed.
    attachGated: visionGated && docGated,
    attachLabel:
      !visionGated && !docGated
        ? "Attach image or document"
        : visionGated
          ? "Attach document"
          : "Attach image",
  };
}
