import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { WorkspaceSkillSummary } from "../../lib/types";

function description(skill: WorkspaceSkillSummary): string {
  // The list describes the latest revision, which may not be the approved one.
  return skill.approved?.digest === skill.latest_digest ? skill.description : "";
}

export default function WorkspaceSkillPicker({ onSelect, onClose }: {
  onSelect: (name: string) => void;
  onClose: (restoreFocus: boolean) => void;
}) {
  const t = useTranslate();
  const listId = useId();
  const search = useRef<HTMLInputElement>(null);
  const [skills, setSkills] = useState<WorkspaceSkillSummary[] | null>();
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  useLayoutEffect(() => { search.current?.focus({ preventScroll: true }); }, []);
  useEffect(() => {
    let current = true;
    void api.workspaceSkills().then(
      ({ skills }) => { if (current) setSkills(skills.filter((skill) => skill.approved)); },
      () => { if (current) setSkills(null); },
    );
    return () => { current = false; };
  }, []);

  const term = query.trim().toLowerCase();
  const matches = skills?.filter((skill) => skill.name.toLowerCase().includes(term) || description(skill).toLowerCase().includes(term)) ?? [];
  const active = matches[index];
  const optionId = (name: string) => `${listId}-${name}`;
  const onKey = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
    if (event.key === "Escape") {
      event.preventDefault();
      onClose(true);
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (active) onSelect(active.name);
    } else if (matches.length && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const next = Math.max(0, Math.min(index + (event.key === "ArrowDown" ? 1 : -1), matches.length - 1));
      setIndex(next);
      document.getElementById(optionId(matches[next].name))?.scrollIntoView({ block: "nearest" });
    }
  };

  return (
    <div className="slashmenu" onBlur={(event) => {
      if (!event.currentTarget.contains(event.relatedTarget)) onClose(false);
    }}>
      <input
        ref={search}
        className="w-full border-b border-ink-700 px-2.5 py-2 text-base sm:text-body focus-visible:outline-accent"
        role="combobox"
        aria-label={t("Search workspace skills")}
        aria-autocomplete="list"
        aria-controls={listId}
        aria-expanded={matches.length > 0}
        aria-activedescendant={active && optionId(active.name)}
        autoComplete="off"
        placeholder={t("Search workspace skills")}
        value={query}
        onChange={(event) => { setQuery(event.target.value); setIndex(0); }}
        onKeyDown={onKey}
      />
      <ul id={listId} role="listbox" aria-label={t("Workspace skills")} hidden={!matches.length}>
        {matches.map((skill, i) => (
          <li key={skill.name} role="presentation">
            <button
              id={optionId(skill.name)}
              type="button"
              role="option"
              tabIndex={-1}
              aria-selected={i === index}
              className={`slashrow${i === index ? " on" : ""}`}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => onSelect(skill.name)}
            >
              <span className="num min-w-0 break-all text-accent">{skill.name}</span>
              {description(skill) && <span className="min-w-0 flex-1 text-ink-500">{description(skill)}</span>}
            </button>
          </li>
        ))}
      </ul>
      {!matches.length && (
        <p className="px-2.5 py-2 text-label text-ink-500" role="status">
          {t(skills === undefined ? "Loading…" : skills === null ? "Workspace skills could not be loaded." : skills.length ? "No matching skills." : "No approved workspace skills.")}
        </p>
      )}
    </div>
  );
}
