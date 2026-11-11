import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiError, api } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import type { TimelineEvent, TimelineKind } from "../../lib/types";
import Link from "../../ui/Link";
import { TIMELINE_KINDS, entryChange, entryLabel, groupByDay, kindCounts, readLastVisit, writeLastVisit } from "./timeline";

type Page =
  | { status: "loading" }
  | { status: "failed"; error: string }
  | { status: "unavailable" }
  | { status: "ok"; events: TimelineEvent[]; next: string | null; since: string | null };

/** The project's Timeline tab (CAD-2240). The tab itself is shown on hosted boards only. */
export default function Timeline({ project, hosted, onOpenIssue, browseHref }: {
  project: string; hosted: boolean; onOpenIssue: (id: string) => void; browseHref: string;
}) {
  const { t, formatDate, formatNumber } = useLocale();
  const [page, setPage] = useState<Page>({ status: "loading" });
  const [openOnly, setOpenOnly] = useState(false);
  const [kind, setKind] = useState<TimelineKind | "all">("all");
  const [attempt, setAttempt] = useState(0);
  const [more, setMore] = useState<"idle" | "loading" | "failed">("idle");
  const lastVisit = useRef<number | null>(null);
  const stamped = useRef<string | null>(null);

  useEffect(() => {
    lastVisit.current = readLastVisit(project);
    stamped.current = null;
    setKind("all");
    setOpenOnly(false);
  }, [project]);

  useEffect(() => {
    const controller = new AbortController();
    // A native board serves no timeline: a stale link says so instead of asking.
    if (!hosted) { setPage({ status: "unavailable" }); return; }
    setPage({ status: "loading" });
    setMore("idle");
    api.projectTimeline(project, { open: openOnly }, controller.signal).then(
      (r) => setPage({ status: "ok", events: r.events, next: r.next_before, since: r.recording_since }),
      (e: unknown) => {
        if (controller.signal.aborted) return;
        // A board that predates the route answers 501: say so, never an empty timeline.
        if (e instanceof ApiError && e.status === 501) setPage({ status: "unavailable" });
        else setPage({ status: "failed", error: e instanceof Error ? e.message : String(e) });
      },
    );
    return () => controller.abort();
  }, [project, hosted, openOnly, attempt]);

  // Remember this visit only after the page has shown, so the marker keeps pointing at the last one.
  useEffect(() => {
    if (page.status !== "ok" || stamped.current === project) return;
    stamped.current = project;
    writeLastVisit(project, Date.now());
  }, [page.status, project]);

  const showEarlier = useCallback(() => {
    if (page.status !== "ok" || !page.next) return;
    setMore("loading");
    api.projectTimeline(project, { open: openOnly, before: page.next }).then(
      (r) => {
        setPage((cur) => cur.status === "ok" && cur.next === page.next
          ? { ...cur, events: [...cur.events, ...r.events.filter((e) => !cur.events.some((x) => x.id === e.id))], next: r.next_before }
          : cur);
        setMore("idle");
      },
      () => setMore("failed"),
    );
  }, [page, project, openOnly]);

  const loaded = page.status === "ok" ? page.events : [];
  const counts = useMemo(() => kindCounts(loaded), [loaded]);
  const shown = useMemo(() => kind === "all" ? loaded : loaded.filter((e) => e.kind === kind), [loaded, kind]);
  const days = useMemo(() => groupByDay(shown, lastVisit.current), [shown]);
  const dayName = (at: string) => formatDate(at, { day: "numeric", month: "short" });

  const changeText = (e: TimelineEvent): string | null => {
    if (e.kind === "status" && e.landed) return t("Landed");
    const c = entryChange(e);
    if (c) return `${c.from ?? t("none")} → ${c.to ?? t("none")}`;
    if (e.kind === "created") return t("Created this ticket");
    return null;
  };

  return <main className="project-overview px-4 lg:px-8 py-6 min-w-0" aria-label={t("Timeline")}>
    <header className="project-page-heading">
      <div style={{ minWidth: 0, flex: 1 }}>
        <div className="slabel">{t("Project timeline")}</div>
        <h1>{project}</h1>
        <p className="tl-lede">{t("What happened, newest first: ticket changes, comments, messages and reviews.")}</p>
      </div>
      <Link className="project-action" href={browseHref}>{t("Browse issues")} <span aria-hidden>↗</span></Link>
    </header>

    {page.status === "loading" && <div className="card px-4 py-5 text-secondary text-ink-400" role="status">{t("Reading the timeline…")}</div>}
    {page.status === "failed" && <div className="card px-4 py-5 text-secondary text-fail border-fail/40 flex flex-wrap items-center gap-3" role="alert">
      <span className="min-w-0">{t("The timeline could not be loaded")} — {page.error}</span>
      <button onClick={() => setAttempt((n) => n + 1)} className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto">{t("retry")}</button>
    </div>}
    {page.status === "unavailable" && <p className="card px-4 py-5 text-secondary text-ink-400" role="note">{t("The timeline is not available on this board yet.")}</p>}

    {page.status === "ok" && <>
      <div className="tl-filters" role="group" aria-label={t("Filter the timeline")}>
        <span className="tl-kinds">
          <button className="chip chip-btn chip-sans" aria-pressed={kind === "all"} onClick={() => setKind("all")}>{t("All")}<span className="count">{formatNumber(loaded.length)}</span></button>
          {TIMELINE_KINDS.map((k) => <button key={k.kind} className="chip chip-btn chip-sans" aria-pressed={kind === k.kind} onClick={() => setKind(k.kind)}>{t(k.label)}<span className="count">{formatNumber(counts[k.kind])}</span></button>)}
        </span>
        <button className="chip chip-btn chip-sans tl-open" aria-pressed={openOnly} onClick={() => setOpenOnly(!openOnly)}>{t("Still open only")}</button>
      </div>
      <p className="text-micro text-ink-500 mb-4" role="status">
        {formatNumber(loaded.length)} {loaded.length === 1 ? t("entry loaded") : t("entries loaded")}
        {page.since && <> · {t("Ticket changes are recorded from")} <time dateTime={page.since}>{formatDate(page.since, { dateStyle: "medium" })}</time></>}
      </p>

      {loaded.length === 0 && <p className="tl-empty">{openOnly ? t("No messages are still open.") : page.since === null ? t("History starts with the next change on this board.") : t("No entries yet.")}</p>}
      {loaded.length > 0 && shown.length === 0 && <p className="tl-empty">{t("No entries of this kind among those loaded.")}{page.next ? ` ${t("Show earlier to look further back.")}` : ""}</p>}

      <div>
        {days.map((day) => <section key={day.key} className="tl-day" aria-label={day.key}>
          <h2 className="tl-date">{dayName(day.at)}<small>{formatDate(day.at, { weekday: "long" })}</small></h2>
          <ul className="tl-list">
            {day.entries.map(({ event: e, hereBefore }) => {
              const change = changeText(e);
              return <Fragment key={e.id}>
                {hereBefore && <li className="tl-here" role="separator">{t("You were last here")}</li>}
                <li className="tl-item" data-kind={e.kind} data-landed={e.landed} data-open={e.open}>
                  <div className="tl-top">
                    <span className="kind">{t(entryLabel(e))}</span>
                    <time className="num" dateTime={e.at}>{formatDate(e.at, { timeStyle: "short" })}</time>
                    {e.kind === "message" && e.to_role && <span>{t("to")} {e.to_role}</span>}
                    {e.kind === "review" && e.decision && <span>{e.decision}</span>}
                    {e.open && <span className="chip bg-warn/10 text-warn push">{e.open_days ? `${t("Open")} ${e.open_days} ${e.open_days === 1 ? t("day") : t("days")}` : t("Open today")}</span>}
                  </div>
                  <p className="tl-what"><button className="lnk num text-micro" onClick={() => onOpenIssue(e.ticket)}>{e.ticket}</button>{e.title}</p>
                  {(change || e.text || e.actor) && <p className="tl-meta">
                    {e.actor?.name}{e.actor && (change || e.text) ? " · " : ""}{change}{change && e.text ? " · " : ""}{e.text}
                  </p>}
                </li>
              </Fragment>;
            })}
          </ul>
        </section>)}
      </div>

      {page.next && <div className="tl-more">
        <button className="chip chip-btn chip-sans" disabled={more === "loading"} onClick={showEarlier}>{more === "loading" ? t("Loading…") : t("Show earlier")}</button>
        {more === "failed" && <span className="text-label text-fail ml-3" role="alert">{t("Earlier entries could not be loaded.")}</span>}
      </div>}
    </>}
  </main>;
}
