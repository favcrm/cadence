import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { IconClose, IconSearch } from "../../ui/icons";
import AgentAvatar, { lookFor } from "../agents/AgentAvatar";
import { slugifyHandle } from "../mentions/mentions";
import {
  addHumanMember,
  searchHumanPeople,
  type HumanAgent,
  type HumanMe,
  type HumanPeoplePage,
  type HumanPerson,
} from "./humanThreadsClient";

export type AddToChannelTab = "people" | "agents";

function errorText(error: unknown): string {
  return error instanceof ApiError || error instanceof Error ? error.message : String(error);
}

/** Unified modal dialog for inviting teammates or adding shared agents to a channel. */
export default function AddToChannelDialog({
  open,
  initialTab = "people",
  me,
  roomId,
  existingMemberIds,
  agents,
  agentsError,
  onAdded,
  onClose,
}: {
  open: boolean;
  initialTab?: AddToChannelTab;
  me: HumanMe;
  roomId: string;
  existingMemberIds: ReadonlySet<string>;
  agents: HumanAgent[] | null;
  agentsError: string | null;
  onAdded: () => void | Promise<void>;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const t = useTranslate();
  const [tab, setTab] = useState<AddToChannelTab>(initialTab);
  const [query, setQuery] = useState("");
  const [selectedPeople, setSelectedPeople] = useState<HumanPerson[]>([]);
  const [selectedAgents, setSelectedAgents] = useState<HumanAgent[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [peopleResult, setPeopleResult] = useState<{
    scope: string;
    page: HumanPeoplePage | null;
    error: string | null;
  } | null>(null);
  const currentScopeRef = useRef("");

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open) {
      setTab(initialTab);
      setQuery("");
      setSelectedPeople([]);
      setSelectedAgents([]);
      setError(null);
      if (!dialog.open) {
        if (typeof dialog.showModal === "function") dialog.showModal();
        if (!dialog.open) dialog.setAttribute("open", "");
      }
    } else if (dialog.open || dialog.hasAttribute("open")) {
      if (typeof dialog.close === "function") dialog.close();
      else dialog.removeAttribute("open");
    }
  }, [open, initialTab]);

  const term = query.trim();
  const scope = `${open ? "open" : "closed"}\n${me.id}\n${tab === "people" ? term : ""}`;
  currentScopeRef.current = scope;
  const activePeople = peopleResult?.scope === scope ? peopleResult : null;
  const peopleLoading = open && tab === "people" && activePeople === null;

  useEffect(() => {
    if (!open || tab !== "people") return;
    const controller = new AbortController();
    let live = true;
    const timer = window.setTimeout(() => {
      void searchHumanPeople(term, controller.signal)
        .then((page) => {
          if (!live || currentScopeRef.current !== scope) return;
          if (page.actor !== me.id) {
            setPeopleResult({
              scope,
              page: null,
              error: t("Your session changed. Reopen Channels to search members."),
            });
            return;
          }
          setPeopleResult({ scope, page, error: null });
        })
        .catch((failure: unknown) => {
          if (live && currentScopeRef.current === scope) {
            setPeopleResult({ scope, page: null, error: errorText(failure) });
          }
        });
    }, term ? 150 : 0);
    return () => {
      live = false;
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [open, tab, me.id, term, scope, t]);

  const visiblePeople = useMemo(
    () => (activePeople?.page?.people ?? []).filter((person) => person.id !== me.id),
    [activePeople?.page?.people, me.id],
  );

  const visibleAgents = useMemo(() => {
    const all = agents ?? [];
    if (!term) return all;
    const needle = term.toLowerCase();
    return all.filter((agent) => agent.alias.toLowerCase().includes(needle));
  }, [agents, term]);

  const togglePerson = (person: HumanPerson) => {
    if (existingMemberIds.has(person.id) || existingMemberIds.has(person.subject)) return;
    setSelectedPeople((current) =>
      current.some((item) => item.subject === person.subject)
        ? current.filter((item) => item.subject !== person.subject)
        : [...current, person],
    );
  };

  const toggleAgent = (agent: HumanAgent) => {
    if (existingMemberIds.has(agent.uid)) return;
    setSelectedAgents((current) =>
      current.some((item) => item.uid === agent.uid)
        ? current.filter((item) => item.uid !== agent.uid)
        : [...current, agent],
    );
  };

  const selectedCount = selectedPeople.length + selectedAgents.length;

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (pending || selectedCount === 0) return;
    setPending(true);
    setError(null);
    const failedPeople: HumanPerson[] = [];
    const failedAgents: HumanAgent[] = [];
    let succeeded = 0;
    let firstError: string | null = null;

    for (const person of selectedPeople) {
      try {
        await addHumanMember(roomId, { human_subject: person.subject });
        succeeded += 1;
      } catch (failure) {
        failedPeople.push(person);
        firstError ??= errorText(failure);
      }
    }
    for (const agent of selectedAgents) {
      try {
        await addHumanMember(roomId, { agent_uid: agent.uid });
        succeeded += 1;
      } catch (failure) {
        failedAgents.push(agent);
        firstError ??= errorText(failure);
      }
    }

    if (succeeded > 0) {
      await onAdded();
    }
    setPending(false);
    if (failedPeople.length === 0 && failedAgents.length === 0) {
      onClose();
      return;
    }
    setSelectedPeople(failedPeople);
    setSelectedAgents(failedAgents);
    setError(firstError);
  };

  return (
    <dialog
      ref={ref}
      className="threads-context-sheet threads-add-dialog"
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault();
        if (!pending) onClose();
      }}
      onClick={(event) => {
        if (event.target === ref.current && !pending) onClose();
      }}
    >
      <div className="threads-context-sheet-head">
        <h2 id={titleId}>{t("Add to channel")}</h2>
        <Button
          variant="ghost"
          size="sm"
          disabled={pending}
          onClick={onClose}
          aria-label={t("Close add to channel")}
          icon={<IconClose />}
        />
      </div>
      <form className="threads-add-dialog-form" onSubmit={(event) => void handleSubmit(event)}>
        <div className="threads-add-tabs" role="tablist" aria-label={t("Member type")}>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "people"}
            disabled={pending}
            onClick={() => {
              setTab("people");
              setQuery("");
            }}
          >
            {t("Teammates")}
            {selectedPeople.length > 0 ? ` (${selectedPeople.length})` : ""}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "agents"}
            disabled={pending}
            onClick={() => {
              setTab("agents");
              setQuery("");
            }}
          >
            {t("Shared agents")}
            {selectedAgents.length > 0 ? ` (${selectedAgents.length})` : ""}
          </button>
        </div>

        <label className="threads-add-search">
          <IconSearch size={14} aria-hidden />
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t(
              tab === "people" ? "Search teammates by name or handle…" : "Search shared agents…",
            )}
            aria-label={t(tab === "people" ? "Search teammates" : "Search shared agents")}
          />
        </label>

        <div className="threads-add-results" role="list">
          {tab === "people" ? (
            peopleLoading ? (
              <p className="threads-note">{t("Searching teammates…")}</p>
            ) : activePeople?.error ? (
              <p className="threads-note" role="status">{activePeople.error}</p>
            ) : visiblePeople.length === 0 ? (
              <p className="threads-note">
                {t(term ? "No teammates match this search." : "No other workspace teammates found.")}
              </p>
            ) : (
              visiblePeople.map((person) => {
                const alreadyInRoom =
                  existingMemberIds.has(person.id) || existingMemberIds.has(person.subject);
                const checked = selectedPeople.some((item) => item.subject === person.subject);
                const slug = slugifyHandle(person.name);
                return (
                  <label
                    key={person.subject}
                    className={`threads-add-choice ${checked ? "is-checked" : ""} ${alreadyInRoom ? "is-existing" : ""}`}
                    role="listitem"
                  >
                    <input
                      type="checkbox"
                      checked={checked || alreadyInRoom}
                      disabled={pending || alreadyInRoom}
                      onChange={() => togglePerson(person)}
                    />
                    <span className="threads-add-choice-avatar" aria-hidden>
                      {(person.name.trim()[0] ?? "?").toUpperCase()}
                    </span>
                    <span className="threads-add-choice-meta">
                      <span className="threads-add-choice-name">{person.name}</span>
                      {slug ? <span className="threads-add-choice-hint">@{slug}</span> : null}
                    </span>
                    {alreadyInRoom ? (
                      <span className="threads-add-choice-tag">{t("In channel")}</span>
                    ) : null}
                  </label>
                );
              })
            )
          ) : agents === null ? (
            <p className="threads-note">{agentsError ?? t("Loading shared agents…")}</p>
          ) : visibleAgents.length === 0 ? (
            <p className="threads-note">
              {t(term ? "No shared agents match this search." : "No shared agents configured yet.")}
            </p>
          ) : (
            visibleAgents.map((agent) => {
              const alreadyInRoom = existingMemberIds.has(agent.uid);
              const checked = selectedAgents.some((item) => item.uid === agent.uid);
              return (
                <label
                  key={agent.uid}
                  className={`threads-add-choice ${checked ? "is-checked" : ""} ${alreadyInRoom ? "is-existing" : ""}`}
                  role="listitem"
                >
                  <input
                    type="checkbox"
                    checked={checked || alreadyInRoom}
                    disabled={pending || alreadyInRoom}
                    onChange={() => toggleAgent(agent)}
                  />
                  <AgentAvatar slug={lookFor(agent.look ?? agent.alias)} size={26} />
                  <span className="threads-add-choice-meta">
                    <span className="threads-add-choice-name">@{agent.alias}</span>
                    <span className="threads-add-choice-hint">{t("Shared agent")}</span>
                  </span>
                  {alreadyInRoom ? (
                    <span className="threads-add-choice-tag">{t("In channel")}</span>
                  ) : null}
                </label>
              );
            })
          )}
        </div>

        {error ? <p className="threads-note" role="status">{error}</p> : null}

        <div className="threads-add-dialog-foot">
          <Button variant="ghost" size="sm" disabled={pending} onClick={onClose}>
            {t("Cancel")}
          </Button>
          <Button
            type="submit"
            variant="primary"
            size="sm"
            disabled={pending || selectedCount === 0}
          >
            {pending
              ? t("Adding…")
              : selectedCount > 0
                ? t("Add ({count})").replace("{count}", String(selectedCount))
                : t("Add selected")}
          </Button>
        </div>
      </form>
    </dialog>
  );
}
