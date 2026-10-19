import { useEffect, useLayoutEffect, useRef } from "react";
import { useLocale } from "../../lib/locale";
import { refreshMyWork, resources } from "../../lib/resources";
import { useResource } from "../../lib/useResource";
import { issuePath } from "../issues/model";
import { PRIORITY_CHIP, STATUS_CHIP } from "../issues/chips";
import AssignmentSummary from "./AssignmentSummary";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";
import { IconChecklist, IconRefresh, IconWarning } from "../../ui/icons";
import "./my-work.css";

/** A caller-scoped task queue; opening a snapshot never changes task progress. */
export default function MyWork({ onOpen, onDetails }: {
  onOpen: (id: string) => void;
  onDetails: (id: string) => void;
}) {
  const { t, formatNumber } = useLocale();
  const state = useResource(resources.myWork);
  useEffect(() => { void resources.myWork.revalidate(); }, []);
  const unavailable = state.status === "failed" || state.status === "stale";
  const loading = state.data === null && !unavailable;
  const rows = unavailable ? [] : state.data?.issues ?? [];
  const mainRef = useRef<HTMLElement>(null);
  const focusedIssue = useRef<string | null>(null);
  useLayoutEffect(() => {
    const id = focusedIssue.current;
    if (!id || rows.some((row) => row.id === id)) return;
    focusedIssue.current = null;
    const main = mainRef.current;
    if (!main || !main.getClientRects().length || !document.hasFocus() || document.activeElement !== document.body) return;
    let target: HTMLElement | null = null;
    main.querySelectorAll<HTMLElement>("[data-my-work-inspect]").forEach((element) => {
      const rect = element.getBoundingClientRect();
      if (!target && rect.bottom >= 0 && rect.top < window.innerHeight) target = element;
    });
    (target ?? main.querySelector<HTMLButtonElement>("button"))?.focus({ preventScroll: true });
  }, [rows]);
  return (
    <main ref={mainRef} className="px-4 py-6 lg:px-8" data-my-work aria-busy={state.inFlight}
      onFocusCapture={(event) => {
        const target = event.target instanceof Element ? event.target : null;
        focusedIssue.current = target?.closest(".my-work-card")?.querySelector<HTMLElement>("[data-my-work-inspect]")?.dataset.myWorkInspect ?? null;
      }}>
      <div className="flex items-center justify-between gap-3">
        <h1 className="text-section font-semibold text-ink-100">{t("My work")}</h1>
        <Button type="button" icon={<IconRefresh className={state.inFlight ? "animate-spin motion-reduce:animate-none" : undefined} />} onClick={() => void refreshMyWork()}>
          {t("Refresh")}
        </Button>
      </div>
      <p className="mt-2 text-sm text-ink-400">
        {t("Your assigned issues. Inspect for a quick snapshot, or open Details for the full issue.")}
      </p>
      {loading ? (
        <PageState title={t("Loading your work")} icon={<IconRefresh size={32} />} />
      ) : unavailable ? (
        <PageState title={t("My work unavailable")} icon={<IconWarning size={32} />}>
          {state.error ?? t("Could not load your work.")}
        </PageState>
      ) : rows.length === 0 ? (
        <PageState title={t("No issues assigned to you")} icon={<IconChecklist size={32} />}>
          <Link href="/projects">{t("Browse projects")}</Link>
        </PageState>
      ) : (
        <ul className="my-work-list">
          {rows.map((issue) => (
            <li key={issue.id} className="card my-work-card">
              <button type="button" className="my-work-open" data-my-work-inspect={issue.id}
                aria-haspopup="dialog" aria-label={t("Inspect {id}: {title}").replace("{id}", issue.id).replace("{title}", issue.title)}
                aria-describedby={`my-work-meta-${issue.id}`} onClick={() => onOpen(issue.id)}>
                <span className="my-work-title">{issue.title}</span>
                <span className="my-work-meta" id={`my-work-meta-${issue.id}`}>
                  <span>{issue.id}</span>
                  <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{t(issue.status)}</span>
                  <span className={`chip ${PRIORITY_CHIP[issue.priority] ?? "bg-ink-800 text-ink-400"}`}>{issue.priority}</span>
                  {issue.blocked && <span className="chip bg-warn/10 text-warn">{t("Blocked")}</span>}
                  <span>{issue.project}</span>
                  {issue.checks.total > 0 && <span>{formatNumber(issue.checks.done)}/{formatNumber(issue.checks.total)} {t(issue.checks.total === 1 ? "check" : "checks")}</span>}
                  {issue.counts.comments > 0 && <span>{formatNumber(issue.counts.comments)} {t(issue.counts.comments === 1 ? "comment" : "comments")}</span>}
                  <span className="my-work-assignment"><AssignmentSummary assignment={issue.assignment} /></span>
                </span>
              </button>
              <Link href={issuePath(issue.project, issue.id)} className="my-work-details"
                aria-label={t("Open full details for {id}").replace("{id}", issue.id)}
                onClick={(event) => {
                  if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) onDetails(issue.id);
                }}>{t("Details")}</Link>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}

