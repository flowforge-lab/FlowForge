// Orchestrates the dev step-timeline download (#417): build the dump from the turn's
// steps, serialize to the chosen format, and write it via the shared file-save core.
// Kept apart from the pure serializer (step-export.ts) so that stays side-effect-free
// and unit-testable.

import {
  buildTimeline,
  timelineToCsv,
  timelineToJson,
  timelineFilename,
  type TimelineMeta,
} from "@/lib/step-export";
import { saveTextToFile, type SaveResult } from "@/lib/save-file";
import type { TurnItem } from "@/lib/turn-groups";

const MIME = {
  json: "application/json",
  csv: "text/csv",
} as const;

/** Build + serialize a turn's step timeline and write it to a user-chosen file. Takes
 *  the turn's interleaved `items` (reasoning + steps in `foldTurns` order, #593). */
export async function downloadStepTimeline(
  items: TurnItem[],
  meta: TimelineMeta,
  format: "json" | "csv",
): Promise<SaveResult> {
  const dump = buildTimeline(items, meta);
  const content =
    format === "json" ? timelineToJson(dump) : timelineToCsv(dump);
  return saveTextToFile(content, {
    defaultFilename: timelineFilename(dump, format),
    extension: format,
    filterName: format.toUpperCase(),
    mime: MIME[format],
  });
}
