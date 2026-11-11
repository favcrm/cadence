import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { ApiError, api } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import type { TimelineEvent, TimelineKind } from "../../lib/types";
import Link from "../../ui/Link";
import { FILTER_KINDS, entryChange, entryLabel, groupByDay, readLastVisit, writeLastVisit } from "./timeline";

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
  const [busy, setBusy] = useState(false);
  const [more, setMore] = useState<"idle" | "loading" | "failed">("idle");
  // The caller remounts this page per project (`key`), so one read per mount is enough.
  const [lastVisit] = useState(() => readLastVisit(project));
  const stamped = useRef(false);
  const statusLine = useRef<HTMLParagraphElement>(null);
  const kinds = kind === "all" ? undefined : [kind];

  useEffect(() => {
    const controller = new AbortController();
    // A native board serves no timeline: a stale link says so instead of asking.
    if (!hosted) { setPage({ status: "unavailable" }); return; }
    // A refetch keeps the current entries and the filter controls mounted (focus stays put).
    setPage((cur) => cur.status === "ok" ? cur : { status: "loading" });
    setBusy(true);
    setMore("idle");
    api.projectTimeline(project, { open: openOnly, kinds }, controller.signal).then(
      (r) => { setBusy(false); setPage({ status: "ok", events: r.events, next: r.next_before, since: r.recording_since }); },
      (e: unknown) => {
        if (controller.signal.aborted) return;
        setBusy(false);
        // A board that predates the route answers 501: say so, never an empty timeline.
        if (e instanceof ApiError && e.status === 501) setPage({ status: "unavailable" });
        else setPage({ status: "failed", error: e instanceof Error ? e.message : String(e) });
      },
    );
    return () => controller.abort();
  }, [project, hosted, openOnly, kind, attempt]);

  // Remember this visit only after the page has shown, so the marker keeps pointing at the last one.
  useEffect(() => {
    if (page.status !== "ok" || stamped.current) return;
    stamped.current = true;
    writeLastVisit(project, Date.now());
  }, [page.status, project]);

  const showEarlier = () => {
    if (page.status !== "ok" || !page.next || more === "loading") return;
    const cursor = page.next;
    setMore("loading");
    api.projectTimeline(project, { open: openOnly, kinds, before: cursor }).then(
      (r) => {
        setPage((cur) => cur.status === "ok" && cur.next === cursor
          ? { ...cur, events: [...cur.events, ...r.events.filter((e) => !cur.events.some((x) => x.id === e.id))], next: r.next_before }
          : cur);
        setMore("idle");
        // The button leaves with the last page; keep keyboard focus on the page.
        if (r.next_before === null) setTimeout(() => statusLine.current?.focus(), 0);
      },
      () => setMore("failed"),
    );
  };

  const loaded = page.status === "ok" ? page.events : [];
  const days = useMemo(() => groupByDay(loaded, lastVisit), [loaded, lastVisit]);
  const dayName = (at: string) => formatDate(at, { day: "numeric", month: "short" });

  const changeText = (e: TimelineEvent): string | null => {
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

    {page.status !== "unavailable" && <div className="tl-filters" role="group" aria-label={t("Filter the timeline")}>
      <span className="tl-kinds">
        <button className="chip chip-btn chip-sans" aria-pressed={kind === "all"} onClick={() => setKind("all")}>{t("All")}</button>
        {FILTER_KINDS.map((k) => <button key={k.kind} className="chip chip-btn chip-sans" aria-pressed={kind === k.kind} onClick={() => setKind(k.kind)}>{t(k.label)}</button>)}
      </span>
      <button className="chip chip-btn chip-sans tl-open" aria-pressed={openOnly} onClick={() => setOpenOnly(!openOnly)}>{t("Still open only")}</button>
    </div>}

    {page.status === "loading" && <div className="card px-4 py-5 text-secondary text-ink-400" role="status">{t("Reading the timeline…")}</div>}
    {page.status === "failed" && <div className="card px-4 py-5 text-secondary text-fail border-fail/40 flex flex-wrap items-center gap-3" role="alert">
      <span className="min-w-0">{t("The timeline could not be loaded")} — {page.error}</span>
      <button onClick={() => setAttempt((n) => n + 1)} className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto">{t("retry")}</button>
    </div>}
    {page.status === "unavailable" && <p className="card px-4 py-5 text-secondary text-ink-400" role="note">{t("The timeline is not available on this board yet.")}</p>}

    {page.status === "ok" && <div aria-busy={busy}>
      <p ref={statusLine} tabIndex={-1} className="text-micro text-ink-500 mb-2" role="status">
        {formatNumber(loaded.length)} {loaded.length === 1 ? t("entry loaded") : t("entries loaded")}
        {page.since && <> · {t("Ticket changes are recorded from")} <time dateTime={page.since}>{formatDate(page.since, { dateStyle: "medium" })}</time></>}
      </p>
      {page.since === null && <p className="text-micro text-ink-500 mb-4" role="note">{t("Ticket changes are not recorded on this board yet. They start with the next change; comments, messages and reviews are shown.")}</p>}

      {loaded.length === 0 && <p className="tl-empty">{openOnly ? t("No messages are still open.") : kind !== "all" ? t("No entries of this kind.") : t("No entries yet.")}</p>}

      <div>
        {days.map((day) => <div key={day.key} className="tl-day">
          <h2 className="tl-date">{dayName(day.at)}<small>{formatDate(day.at, { weekday: "long" })}</small></h2>
          <ul className="tl-list">
            {day.entries.map(({ event: e, hereBefore }) => {
              const change = changeText(e);
              return <Fragment key={e.id}>
                {hereBefore && <li className="tl-here">{t("You were last here")}</li>}
                <li className="tl-item" data-kind={e.kind} data-landed={e.landed} data-open={e.open}>
                  <div className="tl-top">
                    <span className="kind">{t(entryLabel(e))}</span>
                    <time className="num" dateTime={e.at}>{formatDate(e.at, { timeStyle: "short" })}</time>
                    {e.kind === "message" && e.to_role && <span>{t("to")} {e.to_role}</span>}
                    {e.kind === "review" && e.decision && <span>{e.decision}</span>}
                    {e.open && <span className="chip bg-warn/10 text-warn push">{e.open_days ? t(e.open_days === 1 ? "Open {n} day" : "Open {n} days").replace("{n}", String(e.open_days)) : t("Open today")}</span>}
                  </div>
                  <p className="tl-what"><button className="lnk num text-micro" onClick={() => onOpenIssue(e.ticket)}>{e.ticket}</button>{e.title}</p>
                  {(change || e.text || e.actor) && <p className="tl-meta">
                    {e.actor?.name}{e.actor && (change || e.text) ? " · " : ""}{change}{change && e.text ? " · " : ""}{e.text}
                  </p>}
                </li>
              </Fragment>;
            })}
          </ul>
        </div>)}
      </div>

      {page.next && <div className="tl-more">
        <button className="chip chip-btn chip-sans" aria-disabled={more === "loading"} onClick={showEarlier}>{more === "loading" ? t("Loading…") : t("Show earlier")}</button>
        {more === "failed" && <span className="text-label text-fail ml-3" role="alert">{t("Earlier entries could not be loaded.")}</span>}
      </div>}
    </div>}
  </main>;
}
