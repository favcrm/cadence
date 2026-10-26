import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { wiki } from "../features/wiki/api";
import { hitKind, type SearchHit } from "../features/wiki/search";
import { withinScope } from "../features/wiki/scope";
import { issuePath } from "../features/issues/model";
import { contextHref, projectWikiRoot } from "../features/projects/contextRoute";
import { navigate } from "../lib/useLocation";
import type { Route } from "../lib/router";
import type { IssueCard } from "../lib/types";
import type { ResourceState } from "../lib/cache";
import Link from "./Link";
import { IconSearch } from "./icons";
import { useTranslate } from "../lib/locale";

/** Results shown per group; the list discloses when it is cut. */
const LIMIT = 8;
/** Quiet time before a Knowledge query, so typing does not query per key. */
const DEBOUNCE_MS = 250;

/** Fills `{name}` placeholders in a catalog sentence; values are inserted literally. */
function fill(text: string, slots: Record<string, string>): string {
  return text.replace(/\{(\w+)\}/g, (match, name: string) => slots[name] ?? match);
}

/** The route's project (follows the current route) or every accessible project. */
type Choice = "route" | "all";

/** A Wiki reply belongs to one query and one scope; anything else is ignored at render. */
type WikiState =
  | { status: "idle"; key: string }
  | { status: "loading"; key: string }
  | { status: "ok"; key: string; hits: SearchHit[] }
  | { status: "error"; key: string; message: string };

interface Row {
  href: string;
  title: string;
  meta: string;
  snippet?: string;
}

/**
 * Card-field matches, ranked: exact ID, then ID prefix, then title, then
 * owner or tag. Completed, dropped and container cards are included.
 */
function rankIssues(issues: IssueCard[], query: string, project: string | null): IssueCard[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const scored: [number, IssueCard][] = [];
  for (const card of issues) {
    if (project !== null && card.project !== project) continue;
    const id = card.id.toLowerCase();
    let rank = 0;
    if (id === q) rank = 1;
    else if (id.startsWith(q)) rank = 2;
    else if (card.title.toLowerCase().includes(q)) rank = 3;
    else if ([card.owner ?? "", ...(card.tags ?? [])].join(" ").toLowerCase().includes(q)) rank = 4;
    if (rank) scored.push([rank, card]);
  }
  return scored.sort((a, b) => a[0] - b[0]).map(([, card]) => card);
}

/** The project the route is in, or null where none applies. */
function routeProject(route: Route): string | null {
  if (route.screen === "issue") return route.project;
  if (route.screen === "projects") return route.slug;
  return null;
}

/** The card's real status and type, so finished, dropped and container work reads as itself. */
function issueMeta(card: IssueCard, showProject: boolean, t: (english: string) => string): string {
  const parts = [showProject ? card.project : null, t(card.status), card.container ? t("container") : null, card.work?.type ? t(card.work.type) : null];
  return parts.filter(Boolean).join(" · ");
}

/**
 * The header's one search entry point (CAD-1396). Issues come from the loaded
 * card fields; Knowledge comes from the existing Wiki search, scoped to
 * `projects/<key>/` when a project is chosen. Each group keeps its own
 * loading, error and empty state.
 */
export default function HeaderSearch({
  issues,
  route,
  navHref,
  hosted = false,
}: {
  issues: ResourceState<IssueCard[]>;
  route: Route;
  navHref: (route: Route) => string;
  /** A hosted tenant does not serve /api/wiki; the Knowledge group is absent there. */
  hosted?: boolean;
}) {
  const t = useTranslate();
  const currentProject = routeProject(route);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [choice, setChoice] = useState<Choice>("all");
  const [active, setActive] = useState(0);
  const [wikiState, setWikiState] = useState<WikiState>({ status: "idle", key: "" });
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const mobileTriggerRef = useRef<HTMLButtonElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const dialogRef = useRef<HTMLElement | null>(null);
  const returnFocusRef = useRef(false);
  const requestRef = useRef(0);
  const trimmed = query.trim();
  const defaultChoice: Choice = currentProject !== null ? "route" : "all";
  const projectKey = choice === "route" ? currentProject : null;
  const wikiRoot = projectKey !== null ? projectWikiRoot(projectKey) : null;
  const wikiKey = `${trimmed}\u0000${wikiRoot ?? ""}`;

  // Binds the route's default scope in the same event as opening.
  const openSearch = () => {
    setChoice(defaultChoice);
    setOpen(true);
  };

  // Cmd/Ctrl+K opens the dialog, or refocuses it when already open.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        if (open) inputRef.current?.focus();
        else {
          setChoice(defaultChoice);
          setOpen(true);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, defaultChoice]);

  // While open, focus that leaves the dialog (Tab, pointer or script) returns to the query.
  useEffect(() => {
    if (!open) return;
    const onFocusIn = (e: FocusEvent) => {
      const dialog = dialogRef.current;
      if (dialog && e.target instanceof Node && !dialog.contains(e.target)) inputRef.current?.focus();
    };
    document.addEventListener("focusin", onFocusIn);
    return () => document.removeEventListener("focusin", onFocusIn);
  }, [open]);

  // Escape returns focus to the visible trigger once the dialog has unmounted.
  useEffect(() => {
    if (open || !returnFocusRef.current) return;
    returnFocusRef.current = false;
    const trigger = triggerRef.current?.offsetParent !== null ? triggerRef.current : mobileTriggerRef.current;
    trigger?.focus();
  }, [open]);

  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  // Knowledge: debounced; a request id drops replies superseded by a newer query or scope.
  useEffect(() => {
    requestRef.current += 1;
    if (!open || !trimmed || hosted) {
      setWikiState({ status: "idle", key: wikiKey });
      return;
    }
    setWikiState({ status: "loading", key: wikiKey });
    const id = requestRef.current;
    const timer = window.setTimeout(() => {
      wiki
        .search(trimmed, undefined, wikiRoot ?? undefined)
        .then((resp) => {
          if (id !== requestRef.current) return;
          const hits = (resp.hits ?? resp.results ?? []).filter((hit) => wikiRoot === null || withinScope(hit.path, wikiRoot));
          setWikiState({ status: "ok", key: wikiKey, hits });
        })
        .catch((e: unknown) => {
          if (id !== requestRef.current) return;
          setWikiState({ status: "error", key: wikiKey, message: e instanceof Error ? e.message : String(e) });
        });
    }, DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [open, trimmed, wikiRoot, wikiKey, hosted]);

  const issueMatches = useMemo(
    () => rankIssues(issues.data ?? [], trimmed, projectKey),
    [issues.data, trimmed, projectKey],
  );
  const issueShown = issueMatches.slice(0, LIMIT);
  const knowledge = hosted
    ? { status: "idle" as const, key: wikiKey }
    : wikiState.key === wikiKey
      ? wikiState
      : { status: "loading" as const, key: wikiKey };
  const hitCount = knowledge.status === "ok" ? knowledge.hits.length : 0;
  const hitShown = knowledge.status === "ok" ? knowledge.hits.slice(0, LIMIT) : [];

  const issueRows: Row[] = issueShown.map((card) => ({
    href: issuePath(card.project, card.id),
    title: `${card.id} ${card.title}`,
    meta: issueMeta(card, projectKey === null, t),
  }));
  const hitRows: Row[] = hitShown.map((hit) => {
    const project = hit.path.startsWith("projects/") ? hit.path.split("/")[1] : null;
    return {
      href: project !== null
        ? contextHref(project, { screen: "wiki", mode: "browse", path: hit.path, query: null })
        : navHref({ screen: "wiki", mode: "browse", path: hit.path, query: null }),
      title: hit.name,
      meta: `${project ?? t("wiki")} · ${t(hitKind(hit))} · ${hit.path}`,
      snippet: hit.snippet,
    };
  });
  const rows = [...issueRows, ...hitRows];

  useEffect(() => setActive(0), [trimmed, projectKey]);

  const close = () => {
    returnFocusRef.current = true;
    setOpen(false);
  };

  const choose = (row: Row | undefined) => {
    if (!row) return;
    setOpen(false);
    navigate(row.href);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    if (e.key === "Tab") {
      // Tab wraps between the dialog's controls and result links.
      const items = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>("input:not([disabled]), button:not([disabled]), a[href]") ?? []);
      const first = items[0];
      const last = items[items.length - 1];
      if (!first) return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
      return;
    }
    // Arrows and Enter drive the selection only from the query. A focused result link keeps its native Enter, modifier and new-tab behaviour.
    if (e.target !== inputRef.current) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(i + 1, Math.max(rows.length - 1, 0)));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey && rows[active]) {
      e.preventDefault();
      choose(rows[active]);
    }
  };

  const truncated = issueMatches.length > LIMIT || hitCount > LIMIT;

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className="header-search-trigger hidden lg:inline-flex"
        aria-expanded={open}
        onClick={openSearch}
      >
        <IconSearch size={14} />
        <span>{t(hosted ? "Search issues…" : "Search issues and knowledge…")}</span>
        <kbd>Ctrl K</kbd>
      </button>
      <div className="lg:hidden">
        <button
          ref={mobileTriggerRef}
          type="button"
          className="header-icon header-search-icon"
          aria-label={t(hosted ? "Search issues" : "Search issues and knowledge")}
          aria-expanded={open}
          onClick={openSearch}
        >
          <IconSearch size={16} />
        </button>
      </div>

      {open && createPortal(
        <div className="header-search-wrap" role="presentation">
          <div className="header-search-scrim" onClick={close} />
          <section
            ref={dialogRef}
            className="card header-search-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={t(hosted ? "Search issues" : "Search issues and knowledge")}
            onKeyDown={onKeyDown}
          >
            <div className="header-search-controls">
              <input
                ref={inputRef}
                className="field header-search-input"
                type="search"
                role="combobox"
                aria-expanded={rows.length > 0}
                aria-controls="header-search-results"
                aria-activedescendant={rows[active] ? `header-search-row-${active}` : undefined}
                aria-autocomplete="list"
                aria-label={t(hosted ? "Search issues" : "Search issues and knowledge")}
                placeholder={t(hosted ? "Search issues by ID, title, owner or tag…" : "Search issues by ID, title, owner or tag, or knowledge…")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <div className="header-search-scope" role="group" aria-label={t("Search scope")}>
                {currentProject !== null && (
                  <button
                    type="button"
                    className={`header-search-chip${projectKey !== null ? " on" : ""}`}
                    aria-pressed={projectKey !== null}
                    onClick={() => setChoice("route")}
                  >
                    {currentProject}
                  </button>
                )}
                <button
                  type="button"
                  className={`header-search-chip${projectKey === null ? " on" : ""}`}
                  aria-pressed={projectKey === null}
                  onClick={() => setChoice("all")}
                >
                  {t("All projects")}
                </button>
              </div>
            </div>

            {!trimmed ? (
              <p className="header-search-guide text-label text-ink-400">
                {hosted
                  ? t("Type an issue ID, title word, owner or tag. Issue search covers card fields only, not descriptions or comments.")
                  : t("Type an issue ID, title word, owner or tag. Knowledge comes from the Wiki. Issue search covers card fields only, not descriptions or comments.")}
              </p>
            ) : (
              <div id="header-search-results" className="header-search-results">
                <section aria-labelledby="header-search-issues-h">
                  <h3 id="header-search-issues-h" className="slabel">{t("Issues")}</h3>
                  {issues.status === "failed" && issues.data === null ? (
                    <p className="text-label text-fail" role="alert">{issues.error ? fill(t("Issues could not load: {error}"), { error: issues.error }) : t("Issues could not load.")}</p>
                  ) : issues.data === null ? (
                    <p className="text-label text-ink-400">{t("Loading issues…")}</p>
                  ) : (
                    <>
                      {issues.error !== null && (
                        <p className="text-label text-fail" role="alert">
                          {fill(t("Issue list could not refresh: {error}. Matches below come from the last loaded list."), { error: issues.error })}
                        </p>
                      )}
                      {issueMatches.length === 0 ? (
                        <p className="text-label text-ink-400">{projectKey ? fill(t("No issues match “{q}” in {p}."), { q: trimmed, p: projectKey }) : fill(t("No issues match “{q}”."), { q: trimmed })}</p>
                      ) : (
                        <ResultList rows={issueRows} offset={0} active={active} onHover={setActive} onPick={choose} />
                      )}
                    </>
                  )}
                </section>

                {!hosted && <section aria-labelledby="header-search-knowledge-h">
                  <h3 id="header-search-knowledge-h" className="slabel">{t("Knowledge")}{projectKey ? ` · ${projectKey}` : ""}</h3>
                  {knowledge.status === "loading" || knowledge.status === "idle" ? (
                    <p className="text-label text-ink-400">{t("Searching knowledge…")}</p>
                  ) : knowledge.status === "error" ? (
                    <p className="text-label text-fail" role="alert">
                      {fill(t("Knowledge search failed: {message}. Issue matches above are unaffected."), { message: knowledge.message })}
                    </p>
                  ) : hitCount === 0 ? (
                    <p className="text-label text-ink-400">{fill(t("No knowledge matches “{q}”."), { q: trimmed })}</p>
                  ) : (
                    <ResultList rows={hitRows} offset={issueRows.length} active={active} onHover={setActive} onPick={choose} />
                  )}
                </section>}

                {truncated && (
                  <p className="text-label text-ink-400">{fill(t("Showing the first {n} of each group. Refine your search to narrow results."), { n: String(LIMIT) })}</p>
                )}
              </div>
            )}
          </section>
        </div>,
        document.body,
      )}
    </>
  );
}

function ResultList({
  rows,
  offset,
  active,
  onHover,
  onPick,
}: {
  rows: Row[];
  offset: number;
  active: number;
  onHover: (index: number) => void;
  onPick: (row: Row) => void;
}) {
  return (
    <ul className="header-search-list" role="listbox">
      {rows.map((row, i) => {
        const index = offset + i;
        return (
          <li
            key={row.href}
            id={`header-search-row-${index}`}
            role="option"
            aria-selected={active === index}
            className={`header-search-row${active === index ? " is-active" : ""}`}
            onMouseEnter={() => onHover(index)}
            onFocus={() => onHover(index)}
          >
            <Link
              href={row.href}
              className="header-search-link"
              onClick={(e) => {
                if (e.button === 0 && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey) {
                  e.preventDefault();
                  onPick(row);
                }
              }}
            >
              <span className="header-search-title">{row.title}</span>
              <span className="header-search-meta text-micro text-ink-500">{row.meta}</span>
              {row.snippet && <span className="header-search-snippet text-micro text-ink-400">{row.snippet}</span>}
            </Link>
          </li>
        );
      })}
    </ul>
  );
}
