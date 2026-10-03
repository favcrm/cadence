import type { AudienceBaseInput } from "../audienceClient";
import type { AudiencePreview } from "../CrmSegments";

/** One plain-language line for Overview: who the audience is and,
 *  once the Audience tab has counted it, how many can be sent to. */
export function audienceSummary(
  pick: { base: AudienceBaseInput; exclusionListId: string | null },
  preview: AudiencePreview | null,
): string {
  const who =
    pick.base.mode === "all"
      ? "All customers"
      : pick.base.mode === "segment"
        ? `Segment ${pick.base.segmentId}`
        : `${pick.base.customerIds.length} chosen customers`;
  if (preview === null) return `${who} · not counted yet`;
  return `${who} · ${preview.baseCount} match · ${preview.finalCount} can be emailed`;
}
