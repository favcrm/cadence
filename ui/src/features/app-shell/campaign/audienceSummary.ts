import type { AudienceBaseInput } from "../audienceClient";
import type { AudiencePreview } from "../CrmSegments";

/** One plain-language line for Overview: who the audience is and,
 *  once the Audience tab has counted it, how many can be sent to. */
export function audienceSummary(
  pick: { base: AudienceBaseInput; exclusionListId: string | null },
  preview: AudiencePreview | null,
  t: (english: string) => string = (english) => english,
): string {
  const who =
    pick.base.mode === "all"
      ? t("All customers")
      : pick.base.mode === "segment"
        ? `${t("Segment")} ${pick.base.segmentId}`
        : `${pick.base.customerIds.length} ${t("chosen customers")}`;
  if (preview === null) return `${who} ${t("· not counted yet")}`;
  return `${who} · ${preview.baseCount} ${t("match")} · ${preview.finalCount} ${t("can be emailed")}`;
}
