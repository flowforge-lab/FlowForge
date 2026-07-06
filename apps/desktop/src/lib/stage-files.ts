// One staging path for every attachment entry point (#723): region drop
// (session-pane.tsx), paste, and the file picker (input-bar.tsx) all funnel here
// so the capability gate and the rejection feedback behave identically. Pure
// orchestration over the existing helpers — no new file-reading logic.

import {
  classifyForStaging,
  describeRejections,
  fileToAttachment,
  type AttachGate,
  type RejectionReason,
} from "@/lib/attachments";
import { useComposerStore } from "@/store/composer";
import { useAttachRejectToastStore } from "@/store/attach-reject-toast";

/**
 * Stage `files` into `sessionId`'s composer, gated by the resolved model's
 * capabilities. Accepted files are read and appended (async, order-independent —
 * matching the pre-#723 loop); rejected files are collected and summarized into a
 * single dismissible toast stating the reason. Returns the count actually staged.
 */
export function stageFiles(
  sessionId: string,
  files: File[],
  gate: AttachGate,
): number {
  const rejections: RejectionReason[] = [];
  let staged = 0;
  for (const file of files) {
    const verdict = classifyForStaging(file, gate);
    // A stageable file resolves to an AttachmentKind; anything else is a reason.
    if (verdict !== "image" && verdict !== "document") {
      rejections.push(verdict);
      continue;
    }
    staged += 1;
    void fileToAttachment(file).then((att) =>
      useComposerStore.getState().stageAttachment(sessionId, att),
    );
  }
  const notice = describeRejections(rejections);
  if (notice) useAttachRejectToastStore.getState().push(notice);
  return staged;
}
