import { useEffect, useId, useRef, useState } from "react";
import Button from "../../ui/Button";
import { IconClose, IconSearch } from "../../ui/icons";
import { searchHumanPeople, type HumanPerson, type HumanPeoplePage } from "./humanThreadsClient";

/** Shared by human DMs and groups. Selections live outside the search result page. */
export default function HumanMemberPicker({
  actorId, selected, onChange, multiple, disabled = false,
}: {
  actorId: string;
  selected: HumanPerson[];
  onChange: (people: HumanPerson[]) => void;
  multiple: boolean;
  disabled?: boolean;
}) {
  const id = useId();
  const searchRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [retry, setRetry] = useState(0);
  const [result, setResult] = useState<{ query: string; page: HumanPeoplePage | null; error: string | null } | null>(null);
  const term = query.trim();
  const scope = `${actorId}\n${term}\n${retry}`;
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const current = result?.query === scope ? result : null;
  const loading = current === null;

  useEffect(() => {
    const controller = new AbortController();
    let live = true;
    const timer = window.setTimeout(() => {
      void searchHumanPeople(term, controller.signal).then((page) => {
        if (!live || currentScope.current !== scope) return;
        if (page.actor !== actorId) {
          setResult({ query: scope, page: null, error: "Your session changed. Reopen Threads to search members." });
          return;
        }
        setResult({ query: scope, page, error: null });
      }).catch((error: unknown) => {
        if (live && currentScope.current === scope) {
          setResult({ query: scope, page: null, error: error instanceof Error ? error.message : String(error) });
        }
      });
    }, term ? 200 : 0);
    return () => {
      live = false;
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [actorId, term, scope]);

  const remove = (subject: string) => {
    onChange(selected.filter((person) => person.subject !== subject));
    searchRef.current?.focus();
  };
  return (
    <div className="threads-member-picker">
      {selected.length > 0 && (
        <div className="threads-picked" aria-label="Selected members">
          {selected.map((person) => (
            <span className="threads-picked-person" key={person.subject}>
              <span>{person.name}</span>
              <Button variant="ghost" size="sm" disabled={disabled} icon={<IconClose size={12} />}
                aria-label={`Remove ${person.name}`} onClick={() => remove(person.subject)} />
            </span>
          ))}
        </div>
      )}
      <label className="threads-field-label" htmlFor={`${id}-search`}>{multiple ? "Add people" : "Send to"}</label>
      <div className="threads-member-search">
        <IconSearch size={14} />
        <input ref={searchRef} id={`${id}-search`} className="field" type="search" value={query}
          disabled={disabled} placeholder="Search workspace members…" maxLength={200}
          onKeyDown={(event) => { if (event.key === "Enter") event.preventDefault(); }}
          aria-controls={`${id}-results`} onChange={(event) => setQuery(event.target.value)} />
        {query && <Button variant="ghost" size="sm" disabled={disabled} icon={<IconClose size={12} />}
          aria-label="Clear member search" onClick={() => { setQuery(""); searchRef.current?.focus(); }} />}
      </div>
      <div className="threads-member-caption" role="status" aria-live="polite">
        <span>Workspace members</span>
        <span>{loading ? "Searching…" : current.error ? "Unavailable" : current.page?.has_more ? "First 20 matches" : `${current.page?.people.length ?? 0} found`}</span>
      </div>
      <div id={`${id}-results`} className="threads-member-results" aria-busy={loading}>
        {loading ? <p className="threads-member-state" role="status">Searching workspace members…</p>
          : current.error ? <div className="threads-member-state" role="alert"><p>Members unavailable: {current.error}</p><Button variant="secondary" size="sm" disabled={disabled} onClick={() => setRetry((value) => value + 1)}>Retry search</Button></div>
          : current.page?.people.length === 0 ? <div className="threads-member-state"><p>{term ? `No members match “${term}”.` : "No other members in this workspace."}</p>{term && <Button variant="ghost" size="sm" onClick={() => { setQuery(""); searchRef.current?.focus(); }}>Clear search</Button>}</div>
          : current.page?.people.map((person) => {
            const checked = selected.some((pick) => pick.subject === person.subject);
            return (
              <label className="threads-member-row" key={person.subject}>
                <span className="threads-avatar" aria-hidden>{person.name.trim().charAt(0).toUpperCase() || "?"}</span>
                <span className="threads-member-name">{person.name}</span>
                <input type={multiple ? "checkbox" : "radio"} name={`${id}-recipient`} disabled={disabled} checked={checked}
                  onChange={() => onChange(multiple ? checked ? selected.filter((pick) => pick.subject !== person.subject) : [...selected, person] : [person])} />
              </label>
            );
          })}
      </div>
      {current?.page?.has_more && <p className="threads-note">Showing the first 20 matches. Refine your search to find anyone in the workspace.</p>}
    </div>
  );
}
