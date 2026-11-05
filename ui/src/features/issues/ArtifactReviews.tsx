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
  const rows = artifacts.map((artifact) => {
    const reviews = (artifact as Record<string, unknown>).reviews;
    const valid = Array.isArray(reviews) && reviews.every((review) =>
      review && typeof review === "object" && !Array.isArray(review) &&
      typeof review.digest === "string" && /^[0-9a-f]{64}$/.test(review.digest) &&
      (review.decision === "pass" || review.decision === "revise" || review.decision === "blocked")
    );
    // A pass summary without its recorded pass cannot establish review status.
    const complete = valid && (!artifact.passed || reviews.some((review) => review.decision === "pass"));
    return { name: artifact.name as string, passed: artifact.passed as boolean, reviews: complete ? reviews : null };
  });
  const complete = rows.every((row) => row.reviews !== null);
  const passed = rows.filter((row) => row.passed).length;
  return (
    <span>
      <span className="block">
        {complete
          ? t("{passed} of {total} artifacts in this review store have a passing review.")
            .replace("{passed}", () => formatNumber(passed))
            .replace("{total}", () => formatNumber(rows.length))
          : t("Artifact review status is unavailable.")}
      </span>
      {rows.map((artifact) => (
        <span className="block" key={artifact.name}>
          <span className="block">{artifact.name} · {t(artifact.reviews === null
            ? "Review details are unavailable."
            : artifact.passed ? "Passing review recorded" : "No passing review recorded")}</span>
          {artifact.reviews?.map((review, index) => (
            <span className="block break-all" key={`${review.digest}-${index}`}>
              {t(review.decision === "pass" ? "Pass" : review.decision === "revise" ? "Changes requested" : "Review blocked")}
              {typeof review.reviewerName === "string" && review.reviewerName && ` · ${review.reviewerName}`}
              {` · SHA-256 ${review.digest}`}
            </span>
          ))}
        </span>
      ))}
      <span className="block">{t("Review requirements are not determined here.")}</span>
    </span>
  );
}
