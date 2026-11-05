import { useLocale } from "../../lib/locale";

/** Summarize the board's current artifact reviews, not permission to deliver. */
export function ArtifactReviews({ value }: { value: unknown }) {
  const { t, formatNumber } = useLocale();
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return <span>{t("Artifact review status is unavailable.")}</span>;
  }
  const artifacts = (value as Record<string, unknown>).artifacts;
  if (!Array.isArray(artifacts) || !artifacts.every((artifact) =>
    artifact && typeof artifact === "object" && !Array.isArray(artifact) &&
    typeof artifact.name === "string" && typeof artifact.passed === "boolean"
  )) {
    return <span>{t("Artifact review status is unavailable.")}</span>;
  }
  if (artifacts.length === 0) return <span>{t("No artifacts in this review store.")}</span>;
  const passed = artifacts.filter((artifact) => artifact.passed).length;
  return (
    <span>
      <span className="block">
        {t("{passed} of {total} artifacts in this review store have a passing review.")
          .replace("{passed}", () => formatNumber(passed))
          .replace("{total}", () => formatNumber(artifacts.length))}
      </span>
      {artifacts.map((artifact) => {
        const reviews = (artifact as Record<string, unknown>).reviews;
        const valid = Array.isArray(reviews) && reviews.every((review) =>
          review && typeof review === "object" && !Array.isArray(review) &&
          typeof review.digest === "string" && /^[0-9a-f]{64}$/.test(review.digest) &&
          (review.decision === "pass" || review.decision === "revise" || review.decision === "blocked")
        );
        return (
          <span className="block" key={artifact.name}>
            <span className="block">{artifact.name} · {t(artifact.passed ? "Passing review recorded" : "No passing review recorded")}</span>
            {!valid && <span className="block">{t("Review details are unavailable.")}</span>}
            {valid && reviews.map((review, index) => (
              <span className="block break-all" key={`${review.digest}-${index}`}>
                {t(review.decision === "pass" ? "Pass" : review.decision === "revise" ? "Changes requested" : "Review blocked")}
                {typeof review.reviewerName === "string" && review.reviewerName && ` · ${review.reviewerName}`}
                {` · SHA-256 ${review.digest}`}
              </span>
            ))}
          </span>
        );
      })}
      <span className="block">{t("Review requirements are not determined here.")}</span>
    </span>
  );
}
