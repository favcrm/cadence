import { useEffect, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { awayLabel, awaySince, readLastSeen, summaryView, writeLastSeen, type SummaryView } from "./sinceLeft";

type Load =
  | { kind: "loading" }
  | { kind: "ok"; view: SummaryView }
  | { kind: "unavailable" }
  | { kind: "error"; message: string };

const nowSecs = () => Math.floor(Date.now() / 1000);

/**
 * "Since you left" (CAD-328): on a return after at least an hour, a
 * compact card with the daemon's `master_summary` since this browser
 * last had Home open. The last-seen time is kept per browser and
 * refreshed while Home is visible. CAD-1574 keeps it one line: the
 * counts read at a glance and the section detail (with its real issue
 * links) opens on tap — never auto-dismissed, never hidden.
 */
export default function SinceCard({
  onOpenIssue,
  hosted = null,
}: {
  onOpenIssue: (id: string) => void;
  /** True on a hosted board: there is no operator daemon to summarise from. */
  hosted?: boolean | null;
}) {
  const t = useTranslate();
  // Read once, before this visit overwrites it.
  const [since] = useState(() => awaySince(readLastSeen(), nowSecs()));
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [dismissed, setDismissed] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (hosted === true) return;
    const mark = () => {
      if (document.visibilityState === "visible") writeLastSeen(nowSecs());
    };
    mark();
    const timer = setInterval(mark, 60_000);
    const onHide = () => writeLastSeen(nowSecs());
    document.addEventListener("visibilitychange", mark);
    addEventListener("pagehide", onHide);
    return () => {
      clearInterval(timer);
      onHide();
      document.removeEventListener("visibilitychange", mark);
      removeEventListener("pagehide", onHide);
    };
  }, [hosted]);

  useEffect(() => {
    if (hosted === true || since === null) return;
    let live = true;
    api
      .masterSummary(since)
      .then((s) => live && setLoad({ kind: "ok", view: summaryView(s) }))
      .catch((e: ApiError) => {
        if (!live) return;
        if (e.status === 501 || e.status === 404) setLoad({ kind: "unavailable" });
        else setLoad({ kind: "error", message: e.message ?? String(e) });
      });
    return () => {
      live = false;
    };
  }, [since, hosted]);

  // A hosted board has no daemon `master_summary`; the card's premise is the
  // local operator's last-visit span, so it does not render there at all.
  if (hosted === true || since === null || dismissed) return null;
  const away = awayLabel(nowSecs() - since);
  // The collapsed strip's one-line description — a count per section
  // when the summary read, else the load state's own words.
  const description =
    load.kind === "ok"
      ? load.view.quiet
        ? null
        : load.view.sections
            .map((sec) => `${sec.rows === null ? "?" : sec.rows.length} ${t(sec.label)}`)
            .join(" · ")
      : null;
  const expandable = load.kind === "ok" && !load.view.quiet;
  return (
    <section
      className="card steps px-3.5"
      aria-label={t("since you left")}
      data-since-card
      data-open={(expandable && open) || undefined}
    >
      <div className="flex items-center gap-2 min-w-0">
        <h2 className="flex items-center min-w-0 flex-1">
          {(() => {
            const line = (
              <>
                <span
                  className={`steps-caret num shrink-0 ${expandable ? "" : "invisible"}`}
                  aria-hidden
                >
                  ›
                </span>
                <span className="text-secondary font-semibold text-ink-100 shrink-0">{t("Since you left")}</span>
                <span className="text-label text-ink-500 num shrink-0">· {away}</span>
                <span className="text-label min-w-0 truncate flex-1 text-left">
                  {load.kind === "loading" && <span className="text-ink-500">{t("Reading what happened…")}</span>}
                  {load.kind === "unavailable" && (
                    <span className="text-ink-500">
                      {t("The summary needs a daemon with the master (`master_summary`) — not available here.")}
                    </span>
                  )}
                  {load.kind === "error" && (
                    <span className="text-fail break-words">{t("Summary unavailable —")} {load.message}</span>
                  )}
                  {load.kind === "ok" && load.view.quiet && (
                    <span className="text-ink-400">{t("Nothing moved — no plans, tickets, reports or questions.")}</span>
                  )}
                  {description !== null && <span className="text-ink-500">{description}</span>}
                </span>
              </>
            );
            // Only a summary with real sections is a toggle — loading,
            // quiet and unavailable states are the whole strip.
            return expandable ? (
              <button
                type="button"
                className="steps-head"
                aria-expanded={open}
                onClick={() => setOpen((o) => !o)}
              >
                {line}
              </button>
            ) : (
              <div className="steps-head cursor-default">{line}</div>
            );
          })()}
        </h2>
        <button
          className="text-label text-ink-500 hover:text-ink-200 shrink-0 self-center"
          onClick={() => setDismissed(true)}
          aria-label={t("dismiss since you left")}
        >
          ✕
        </button>
      </div>
      {load.kind === "ok" && load.view.backlog > 0 && (
        <p className="text-micro text-warn pb-2 pl-6">
          {load.view.backlog} {load.view.backlog === 1 ? t("report") : t("reports")} {t("still queued for the master.")}
        </p>
      )}
      {expandable && (
        <div className="steps-body" aria-hidden={!open} inert={!open}>
          <div className="min-h-0 overflow-hidden">
            <dl className="grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-1.5 pb-3">
              {load.view.sections.map((sec) => (
                <div key={sec.label} className="min-w-0">
                  <dt className="slabel">
                    {t(sec.label)} <span className="num">{sec.rows === null ? "?" : sec.rows.length}</span>
                  </dt>
                  <dd className="text-label text-ink-300 min-w-0">
                    {sec.rows === null && <span className="text-ink-500">{t("unknown")}</span>}
                    {sec.rows?.slice(0, 3).map((r) => (
                      <div key={r.key} className="truncate">
                        {r.issue ? (
                          <button className="lnk num block max-w-full truncate text-left" onClick={() => onOpenIssue(r.issue!)}>
                            {r.text}
                          </button>
                        ) : (
                          r.text
                        )}
                      </div>
                    ))}
                    {sec.rows && sec.rows.length > 3 && (
                      <div className="text-ink-500">+{sec.rows.length - 3} {t("more")}</div>
                    )}
                  </dd>
                </div>
              ))}
            </dl>
          </div>
        </div>
      )}
    </section>
  );
}
