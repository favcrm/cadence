import { useEffect, useState } from "react";
import type { Route } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { wiki } from "./api";
import { filterHits, snippetParts, TYPE_FILTERS, type SearchHit, type TypeFilter } from "./search";
import { Crumbs, EmptyCard, Failure, Loading } from "./shared";
import type { WikiRoute } from "./Wiki";

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
      .search(query)
      .then((resp) => {
        if (!cancelled) setHits(resp.hits ?? resp.results ?? []);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [query]);

  const shown = hits ? filterHits(hits, filter) : null;

  return (
    <>
      <div className="wk-bar">
        <Crumbs
          path=""
          hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })}
        />
        <form
          className="wk-sinput"
          onSubmit={(e) => {
            e.preventDefault();
            navigate(navHref({ screen: "wiki", mode: "search", path: null, query: value.trim() || null }));
          }}
        >
          <input
            className="field"
            value={value}
            aria-label="search the wiki"
            placeholder="search pages and files…"
            onChange={(e) => setValue(e.target.value)}
          />
        </form>
        <div className="wk-tools">
          {TYPE_FILTERS.map((type) => (
            <button
              key={type}
              type="button"
              className={`wk-fchip${filter === type ? " on" : ""}`}
              aria-pressed={filter === type}
              onClick={() => setFilter(type)}
            >
              {type}
            </button>
          ))}
        </div>
      </div>

      {error !== null ? (
        <Failure what="search failed" error={error} />
      ) : !query ? (
        <EmptyCard title="Search the wiki">
          Type a word or path above — results carry the path, the file and the matching line.
        </EmptyCard>
      ) : shown === null ? (
        <Loading what={`searching for “${query}”…`} />
      ) : shown.length === 0 ? (
        <EmptyCard title="No matches">
          nothing in the wiki matches <span className="num">{query}</span>
          {filter !== "all" ? ` among ${filter}` : ""}.
        </EmptyCard>
      ) : (
        <div className="wk-sres">
          {shown.map((hit) => (
            <div key={hit.path} className="card wk-srow">
              <div className="wk-spath num">{hit.path}</div>
              <a
                className="wk-sname lnk"
                href={navHref({ screen: "wiki", mode: "browse", path: hit.path, query: null })}
              >
                {hit.name}
              </a>
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
