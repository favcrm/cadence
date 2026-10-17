import { useEffect, useState } from "react";
import { api, type MyWorkPayload } from "../../lib/api";
import { issuePath } from "../issues/model";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";

/** Personal PM work. No local agent, repository or provider is required. */
export default function MyWork() {
  const t = useTranslate();
  const [refresh, setRefresh] = useState(0);
  const [payload, setPayload] = useState<MyWorkPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setPayload(null);
    setError(null);
    api.myWork(controller.signal).then((body) => {
      if (controller.signal.aborted) return;
      setPayload(body);
      setLoading(false);
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      setError(error instanceof Error ? error.message : t("Could not load your work."));
      setLoading(false);
    });
    return () => controller.abort();
  }, [refresh, t]);

  return (
    <div className="px-4 py-6 lg:px-8" aria-busy={loading}>
      <div className="flex items-center justify-between gap-3">
        <h1 className="text-section font-semibold text-ink-100">{t("My work")}</h1>
        <Button type="button" onClick={() => setRefresh((value) => value + 1)}>
          {t("Refresh")}
        </Button>
      </div>
      <p className="mt-2 text-sm text-ink-300">
        {t("Issues assigned to you. Open an issue for requirements, comments and progress.")}
      </p>
      {loading ? (
        <PageState title={t("Loading your work")} />
      ) : error ? (
        <PageState title={t("My work unavailable")}>{error}</PageState>
      ) : payload?.issues.length === 0 ? (
        <PageState title={t("No issues assigned to you")}>
          <Link href="/projects">{t("Browse projects")}</Link>
        </PageState>
      ) : (
        <ul className="mt-6 space-y-3">
          {payload?.issues.map((issue) => (
            <li key={issue.id} className="card p-3">
              <div className="flex flex-wrap items-center gap-2 text-label">
                <Link href={issuePath(issue.project, issue.id)} className="num text-ink-300">{issue.id}</Link>
                <span className="chip">{t(issue.status)}</span>
                <span className="text-ink-400">{issue.priority} · {issue.project}</span>
                {issue.blocked && <span className="chip text-warn">{t("Blocked")}</span>}
              </div>
              <h2 className="mt-2 text-secondary break-words">
                <Link href={issuePath(issue.project, issue.id)}>{issue.title}</Link>
              </h2>
              <p className="mt-2 text-label text-ink-400">
                {issue.counts.comments} {t("comments")} · {issue.checks.done}/{issue.checks.total} {t("checks")}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

