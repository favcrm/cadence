import { useEffect, useState } from "react";
import type { Route } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { wiki } from "./api";
import { filterHits, snippetParts, TYPE_FILTERS, type SearchHit, type TypeFilter } from "./search";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import { EmptyCard, Failure, Loading, useWikiScope, WikiToolbar } from "./shared";
import type { WikiRoute } from "./Wiki";
import { withinScope } from "./scope";

const FILTER_LABELS: Record<TypeFilter, string> = { all: "All", pages: "Pages", images: "Images", video: "Video", pdf: "PDF" };

/**
 * Search (CAD-581): the query lives in the URL (`/wiki/search/<query>`),
 * the type chips filter the hits client-side, and every result renders
 * its path, name and snippet with the query's occurrences marked.
 */

export default function SearchPane({
  route,
  navHref,
}: {
  route: WikiRoute;
  navHref: (route: Route) => string;
}) {
  const scope = useWikiScope();
  const query = route.query ?? "";
  const [value, setValue] = useState(query);
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<TypeFilter>("all");

  useEffect(() => setValue(query), [query]);

  useEffect(() => {
    if (!query) {
      setHits(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setHits(null);
    setError(null);
    wiki
      .search(query, undefined, scope?.root)
      .then((resp) => {
        if (!cancelled) setHits((resp.hits ?? resp.results ?? []).filter((hit) => !scope || withinScope(hit.path, scope.root)));
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [query, scope?.root]);

  const shown = hits ? filterHits(hits, filter) : null;

  return (
    <>
      <WikiToolbar path="" navHref={navHref} label="Search" actions={<>
          {TYPE_FILTERS.map((type) => (
            <button
              key={type}
              type="button"
              className={`wk-fchip${filter === type ? " on" : ""}`}
              aria-pressed={filter === type}
              onClick={() => setFilter(type)}
            >
              {FILTER_LABELS[type]}
            </button>
          ))}
      </>}>
        <form
          className="wk-sinput"
          onSubmit={(e) => {
            e.preventDefault();
            navigate(navHref({ screen: "wiki", mode: "search", path: null, query: value.trim() || null }));
          }}
        >
          <input
            className="field"
            type="search"
            name="q"
            enterKeyHint="search"
            value={value}
            aria-label={scope ? "Search project context" : "Search the wiki"}
            placeholder="Search pages and files…"
            onChange={(e) => setValue(e.target.value)}
          />
          <Button type="submit">Search</Button>
        </form>
      </WikiToolbar>

      {error !== null ? (
        <Failure what="search failed" error={error} />
      ) : !query ? (
        <EmptyCard title={scope ? "Search project context" : "Search the wiki"}>
          Enter a word or path, then choose Search.
        </EmptyCard>
      ) : shown === null ? (
        <Loading what={`searching for “${query}”…`} />
      ) : shown.length === 0 ? (
        <EmptyCard title="No matches">
          No page matches <span className="num">{query}</span>
          {filter !== "all" ? ` among ${filter}` : ""}.
        </EmptyCard>
      ) : (
        <div className="wk-sres">
          {shown.map((hit, index) => (
            <div key={`${hit.path}:${hit.line ?? index}`} className="card wk-srow">
              <div className="wk-spath num">{scope ? hit.path.slice(scope.root.length + 1) : hit.path}{hit.line ? ` · line ${hit.line}` : ""}</div>
              <Link
                className="wk-sname lnk"
                href={navHref({ screen: "wiki", mode: "browse", path: hit.path, query: null })}
              >
                {hit.name}
              </Link>
              <div className="wk-snip">
                {snippetParts(hit.snippet, query).map((part, i) =>
                  part.mark ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
