import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { MyWorkIssue } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import {
  MY_WORK_PRIORITIES,
  MY_WORK_SEARCH_MAX,
  MY_WORK_SORTS,
  MY_WORK_TABS,
  myWorkFiltered,
  myWorkViewKey,
  type MyWorkPriority,
  type MyWorkSort,
  type MyWorkTab,
  type MyWorkView,
} from "../../lib/myWorkQuery";
import { loadMoreMyWork, MY_WORK_PAGE, refreshMyWork, resources, showMyWorkView } from "../../lib/resources";
import { useResource } from "../../lib/useResource";
import { issuePath } from "../issues/model";
import { PRIORITY_CHIP, STATUS_CHIP } from "../issues/chips";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";
import { Select } from "../../ui/Select";
import { IconChecklist, IconRefresh, IconSearch, IconWarning } from "../../ui/icons";
import "./my-work.css";

const TAB_LABEL: Record<MyWorkTab, string> = { active: "Active", done: "Done", dropped: "Dropped", all: "All" };
const SORT_LABEL: Record<MyWorkSort, string> = { updated: "Updated", priority: "Priority", created: "Created" };
/** The Active tab's groups, in server order. */
const GROUPS = ["doing", "review", "ready", "backlog"];
const SEARCH_DEBOUNCE_MS = 250;

/** A caller-scoped task queue; opening a snapshot never changes task progress. */
export default function MyWork({ view, onView, onOpen, onDetails }: {
  view: MyWorkView;
  onView: (view: MyWorkView) => void;
  onOpen: (id: string) => void;
  onDetails: (id: string) => void;
}) {
  const { t, formatNumber, formatRelativeTime } = useLocale();
  const state = useResource(resources.myWork);
  const viewKey = myWorkViewKey(view);
  useEffect(() => { showMyWorkView(view); }, [viewKey]);
  const tab: MyWorkTab = view.tab ?? "active";
  const current = state.data?.viewKey === viewKey ? state.data : null;
  const unavailable = state.status === "failed" || state.status === "stale";
  const loading = current === null && !unavailable;
  const rows = unavailable || !current ? [] : current.issues;
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreFailed, setMoreFailed] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());

  // Search: typed text applies after a pause; the URL holds what was applied.
  const [draft, setDraft] = useState(view.search ?? "");
  const applied = useRef(view.search ?? "");
  useEffect(() => {
    if ((view.search ?? "") !== applied.current) {
      applied.current = view.search ?? "";
      setDraft(applied.current);
    }
  }, [view.search]);
  useEffect(() => {
    const text = draft.trim();
    if (text === (view.search ?? "")) return;
    // Restarted on every view change, so a tab or filter picked while typing
    // is merged into, never overwritten by, the search that applies.
    const timer = window.setTimeout(() => {
      applied.current = text;
      onView({ ...view, search: text || undefined });
    }, SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [draft, viewKey]);
  const searchRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target;
      if (target instanceof HTMLElement && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName))) return;
      if (document.querySelector("dialog[open]")) return;
      event.preventDefault();
      searchRef.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => { setMoreFailed(false); }, [viewKey]);
  const loadMore = () => {
    setLoadingMore(true);
    setMoreFailed(false);
    loadMoreMyWork()
      .catch(() => setMoreFailed(true))
      .finally(() => setLoadingMore(false));
  };

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

  const filtered = myWorkFiltered(view);
  const clearFilters = () => {
    applied.current = "";
    setDraft("");
    onView({ ...(view.tab ? { tab: view.tab } : {}), ...(view.sort ? { sort: view.sort } : {}) });
  };
  const counts = current?.counts;
  const projectOptions = [
    { value: "", label: t("All projects") },
    ...(current?.projects ?? (view.project ? [view.project] : [])).map((key) => ({ value: key, label: key })),
  ];

  const card = (issue: MyWorkIssue) => (
    <li key={issue.id} className="card my-work-card">
      <button type="button" className="my-work-open" data-my-work-inspect={issue.id}
        aria-haspopup="dialog" aria-label={t("Inspect {id}: {title}").replace("{id}", issue.id).replace("{title}", issue.title)}
        aria-describedby={`my-work-meta-${issue.id}`} onClick={() => onOpen(issue.id)}>
        <span className="my-work-title">{issue.title}</span>
        <span className="my-work-meta" id={`my-work-meta-${issue.id}`}>
          <span>{issue.id}</span>
          {tab !== "active" && <span className={`chip ${STATUS_CHIP[issue.status] ?? "bg-ink-800 text-ink-400"}`}>{t(issue.status)}</span>}
          <span className={`chip ${PRIORITY_CHIP[issue.priority] ?? "bg-ink-800 text-ink-400"}`}>{issue.priority}</span>
          {issue.blocked && <span className="chip bg-warn/10 text-warn">{t("Blocked")}</span>}
          <span>{issue.project}</span>
          {issue.checks.total > 0 && <span>{formatNumber(issue.checks.done)}/{formatNumber(issue.checks.total)} {t(issue.checks.total === 1 ? "check" : "checks")}</span>}
          {issue.counts.comments > 0 && <span>{formatNumber(issue.counts.comments)} {t(issue.counts.comments === 1 ? "comment" : "comments")}</span>}
          {issue.updated && <time className="my-work-updated" dateTime={issue.updated} title={issue.updated}>{formatRelativeTime(issue.updated)}</time>}
        </span>
      </button>
      <Link href={issuePath(issue.project, issue.id)} className="my-work-details"
        aria-label={t("Open full details for {id}").replace("{id}", issue.id)}
        onClick={(event) => {
          if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) onDetails(issue.id);
        }}>{t("Details")}</Link>
    </li>
  );

  const list = () => {
    if (tab !== "active") return <ul className="my-work-list">{rows.map(card)}</ul>;
    return (
      <div className="my-work-groups">
        {GROUPS.map((status) => {
          const groupRows = rows.filter((row) => row.status === status);
          const total = current?.status_counts[status] ?? groupRows.length;
          if (total === 0) return null;
          const open = !collapsed.has(status);
          const hidden = total - groupRows.length;
          return (
            <section key={status} className="my-work-group" aria-labelledby={`my-work-group-${status}`}>
              <h2 className="my-work-group-head" id={`my-work-group-${status}`}>
                <button type="button" aria-expanded={open} onClick={() => setCollapsed((previous) => {
                  const next = new Set(previous);
                  if (open) next.add(status); else next.delete(status);
                  return next;
                })}>
                  <span aria-hidden="true" className="my-work-caret">{open ? "▾" : "▸"}</span>
                  {t(status)} <span className="num">{formatNumber(total)}</span>
                </button>
              </h2>
              {open && groupRows.length > 0 && <ul className="my-work-list">{groupRows.map(card)}</ul>}
              {open && hidden > 0 && (
                <p className="my-work-group-more">{t("{n} more load below").replace("{n}", formatNumber(hidden))}</p>
              )}
            </section>
          );
        })}
      </div>
    );
  };

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

      <div className="my-work-toolbar" role="search" aria-label={t("Find my issues")}>
        <label className="my-work-search" htmlFor="my-work-search">
          <IconSearch />
          <span className="sr-only">{t("Search my issues")}</span>
          <input
            ref={searchRef}
            id="my-work-search"
            type="search"
            value={draft}
            maxLength={MY_WORK_SEARCH_MAX}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={t("Search id or title…")}
            aria-keyshortcuts="/"
          />
          <kbd aria-hidden="true">/</kbd>
        </label>
        <Select
          aria-label={t("Project")}
          value={view.project ?? ""}
          options={projectOptions}
          searchable={projectOptions.length > 8}
          onChange={(value) => onView({ ...view, project: value || undefined })}
        />
        <Select
          aria-label={t("Priority")}
          value={view.priority ?? ""}
          options={[{ value: "", label: t("All priorities") }, ...MY_WORK_PRIORITIES.map((priority) => ({ value: priority, label: priority }))]}
          onChange={(value) => onView({ ...view, priority: (value || undefined) as MyWorkPriority | undefined })}
        />
        <Button type="button" aria-pressed={view.blocked === true} onClick={() => onView({ ...view, blocked: view.blocked ? undefined : true })}>
          {t("Blocked only")}
        </Button>
        <Select
          aria-label={t("Sort")}
          value={view.sort ?? "updated"}
          options={MY_WORK_SORTS.map((sort) => ({ value: sort, label: `${t("Sort")}: ${t(SORT_LABEL[sort])}` }))}
          onChange={(value) => onView({ ...view, sort: value === "updated" ? undefined : value as Exclude<MyWorkSort, "updated"> })}
        />
      </div>

      <div className="my-work-tabs" role="group" aria-label={t("Issue status")}>
        {MY_WORK_TABS.map((value) => (
          <button key={value} type="button" className="my-work-tab" aria-pressed={tab === value}
            onClick={() => onView({ ...view, tab: value === "active" ? undefined : value })}>
            {t(TAB_LABEL[value])}
            <span className="num">{counts ? formatNumber(counts[value]) : "—"}</span>
          </button>
        ))}
      </div>

      {loading ? (
        <PageState title={t("Loading your work")} icon={<IconRefresh size={32} />} />
      ) : unavailable ? (
        <PageState title={t("My work unavailable")} icon={<IconWarning size={32} />}>
          {state.error ?? t("Could not load your work.")}
        </PageState>
      ) : rows.length === 0 ? (
        filtered ? (
          <PageState title={t("No issues match")} icon={<IconSearch size={32} />}>
            <Button type="button" variant="ghost" onClick={clearFilters}>{t("Clear filters")}</Button>
          </PageState>
        ) : counts && counts.all > 0 ? (
          <PageState title={t("Nothing here")} icon={<IconChecklist size={32} />}>
            {t("No issues of yours in this tab.")}
          </PageState>
        ) : (
          <PageState title={t("No issues assigned to you")} icon={<IconChecklist size={32} />}>
            <Link href="/projects">{t("Browse projects")}</Link>
          </PageState>
        )
      ) : (
        <>
          {list()}
          <div className="my-work-footer">
            <span role="status">
              {t("Showing {shown} of {total}")
                .replace("{shown}", formatNumber(rows.length))
                .replace("{total}", formatNumber(current?.total ?? rows.length))}
            </span>
            {filtered && <Button type="button" variant="ghost" size="sm" onClick={clearFilters}>{t("Clear filters")}</Button>}
            {current?.next_cursor && (
              <Button type="button" loading={loadingMore} disabled={state.inFlight} onClick={loadMore}>
                {t("Load {n} more").replace("{n}", formatNumber(Math.min(MY_WORK_PAGE, (current?.total ?? 0) - rows.length)))}
              </Button>
            )}
            {moreFailed && <span className="text-warn" role="alert">{t("Could not load more. Try again.")}</span>}
          </div>
        </>
      )}
    </main>
  );
}
