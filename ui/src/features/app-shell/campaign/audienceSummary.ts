import type { AudienceBaseInput } from "../audienceClient";
import type { AudiencePreview } from "../CrmSegments";

/** One plain-language line for Overview: who the audience is and,
 *  once the Audience tab has counted it, how many can be sent to. */
export function audienceSummary(
  pick: { base: AudienceBaseInput; exclusionListId: string | null },
  preview: AudiencePreview | null,
  t: (english: string) => string = (english) => english,
): string {
  const base = pick.base;
  const who =
    base.mode === "all"
      ? t("All customers")
      : base.mode === "segment"
        ? t("Segment {id}").replace("{id}", () => base.segmentId)
        : t("{count} chosen customers").replace("{count}", () => String(base.customerIds.length));
  if (preview === null) return `${who} ${t("· not counted yet")}`;
  return `${who} · ${t("{base} match · {final} can be emailed").replace("{base}", () => String(preview.baseCount)).replace("{final}", () => String(preview.finalCount))}`;
}
