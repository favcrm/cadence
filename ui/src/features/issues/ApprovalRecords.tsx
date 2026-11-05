import { activityTimeMs, fmtTime } from "../../lib/fmt";
import { useLocale } from "../../lib/locale";

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

/** PR subjects explicitly returned by the approval owner, not guessed from text. */
export function approvalPullRequests(value: unknown): { label: string; href: string }[] {
  const data = object(value);
  if (data?.version !== "hosted-cadence-issue-approvals.v1" ||
      data.scope !== "hosted-project-record" || !Array.isArray(data.heads)) return [];
  const seen = new Set<string>();
  const result: { label: string; href: string }[] = [];
  for (const value of data.heads) {
    const head = object(value);
    if (!head || typeof head.repo !== "string" ||
        !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(head.repo) ||
        typeof head.pr !== "number" || !Number.isSafeInteger(head.pr) || head.pr < 1) continue;
    const label = `${head.repo}#${head.pr}`;
    if (seen.has(label)) continue;
    seen.add(label);
    result.push({ label, href: `https://github.com/${head.repo}/pull/${head.pr}` });
  }
  return result;
}

/** Exact hosted project/head records, not an assessment of current merge authority. */
export function ApprovalRecords({ value }: { value: unknown }) {
  const { t } = useLocale();
  const data = object(value);
  if (data?.version !== "hosted-cadence-issue-approvals.v1" ||
      data.scope !== "hosted-project-record" || !Array.isArray(data.heads)) {
    return <span>{t("Approval requirements and current decisions are unavailable here.")}</span>;
  }
  const heads = data.heads.map(object);
  if (heads.some((head) => !head || typeof head.head_sha !== "string" ||
      !/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(head.head_sha) ||
      typeof head.repo !== "string" || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(head.repo) ||
      typeof head.pr !== "number" || !Number.isSafeInteger(head.pr) || head.pr < 1 ||
      typeof head.state !== "string" || !["in-force", "missing", "unavailable"].includes(head.state) ||
      (head.state === "in-force" && head.verified !== false))) {
    return <span>{t("Approval requirements and current decisions are unavailable here.")}</span>;
  }
  if (heads.length === 0) return <span>{t(data.available === true
    ? "No hosted project approval records were returned in this bounded view."
    : "Approval records are unavailable or incomplete.")} {t("Current head, approval requirements, revocation and authority are not evaluated here.")}</span>;
  return <div className="grid gap-2">
    {heads.map((item) => {
      const head = item!;
      const at = typeof head.recorded_at === "string" && activityTimeMs(head.recorded_at)
        ? head.recorded_at
        : null;
      const label = head.state === "in-force"
        ? "Hosted approval in force for this recorded head"
        : head.state === "missing"
          ? "No approval record for this recorded head"
          : "Approval record unavailable for this recorded head";
      return <div key={`${head.repo}:${head.pr}:${head.head_sha}`}>
        <div>{t(label)} · <a href={`https://github.com/${head.repo}/pull/${head.pr}`} target="_blank" rel="noopener noreferrer">{`${head.repo}#${head.pr}`}</a> · <span className="num" title={head.head_sha as string}>{(head.head_sha as string).slice(0, 12)}</span>{at && ` · ${fmtTime(at)}`}</div>
        {head.state === "in-force" && <div>{t("Approval verification: unverified")}</div>}
        {head.state === "in-force" && typeof head.recorded_via === "string" && (
          <div>{t("Recorded via")}: {head.recorded_via}</div>
        )}
        {head.state === "in-force" && typeof head.source === "string" && (
          <div className="whitespace-pre-wrap">{t("Recorded source")}: {head.source}</div>
        )}
      </div>;
    })}
    {data.available !== true && <span>{t("Approval records are unavailable or incomplete.")}</span>}
    {data.heads_truncated === true && <span>{t("Additional recorded heads are not shown.")}</span>}
    <span className="dim">{t("Only currently readable, bounded project records are shown; historical or additional records may be unavailable.")}</span>
    <span className="dim">{t("Current head, approval requirements, revocation and authority are not evaluated here.")}</span>
  </div>;
}
