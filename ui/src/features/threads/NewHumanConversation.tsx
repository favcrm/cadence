import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { IconClose } from "../../ui/icons";
import { newMessageId } from "../home/thread";
import { slugifyHandle } from "../mentions/mentions";
import {
  createHumanGroup,
  openHumanDm,
  searchHumanPeople,
  type HumanAgent,
  type HumanMe,
  type HumanPeoplePage,
  type HumanPerson,
} from "./humanThreadsClient";

export default function NewHumanConversation({
  mode,
  me,
  agents,
  agentsError,
  existingDmPeers,
  onCreated,
  onCancel,
  onBusy,
}: {
  mode: "direct" | "group";
  onBusy: (busy: boolean) => void;
  me: HumanMe;
  agents: HumanAgent[] | null;
  agentsError: string | null;
  existingDmPeers?: ReadonlySet<string>;
  onCreated: (id: string) => void;
  onCancel: () => void;
}) {
  const id = useId();
  const t = useTranslate();
  const [selected, setSelected] = useState<HumanPerson[]>([]);
  const [selectedAgents, setSelectedAgents] = useState<HumanAgent[]>([]);
  const [title, setTitle] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [personQuery, setPersonQuery] = useState("");
  const [peopleRetry, setPeopleRetry] = useState(0);
  const [peopleResult, setPeopleResult] = useState<{
    scope: string;
    page: HumanPeoplePage | null;
    error: string | null;
  } | null>(null);
  const knownPeopleRef = useRef<Map<string, HumanPerson>>(new Map());
  const envelope = useRef<{ key: string; id: string } | null>(null);
  const group = mode === "group";

  const term = personQuery.trim();
  const scope = `${me.id}\n${term}\n${peopleRetry}`;
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const activePeople = peopleResult?.scope === scope ? peopleResult : null;
  const peopleLoading = activePeople === null;

  useEffect(() => {
    const controller = new AbortController();
    let live = true;
    const timer = window.setTimeout(() => {
      void searchHumanPeople(term, controller.signal)
        .then((page) => {
          if (!live || currentScope.current !== scope) return;
          if (page.actor !== me.id) {
            setPeopleResult({
              scope,
              page: null,
              error: t("Your session changed. Reopen Channels to search members."),
            });
            return;
          }
          for (const person of page.people) {
            knownPeopleRef.current.set(person.subject, person);
          }
          setPeopleResult({ scope, page, error: null });
        })
        .catch((failure: unknown) => {
          if (live && currentScope.current === scope) {
            setPeopleResult({
              scope,
              page: null,
              error: failure instanceof Error ? failure.message : String(failure),
            });
          }
        });
    }, term ? 150 : 0);
    return () => {
      live = false;
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [me.id, term, scope, t]);

  const peopleList = useMemo(
    () => (activePeople?.page?.people ?? []).filter((p) => p.id !== me.id),
    [activePeople?.page?.people, me.id],
  );

  const peopleOptions = useMemo(() => {
    const visiblePeople =
      !group && selected[0] && !peopleList.some((p) => p.subject === selected[0].subject)
        ? [selected[0], ...peopleList]
        : peopleList;
    return visiblePeople.map((person) => {
      const slug = slugifyHandle(person.name);
      const alreadyPicked = group && selected.some((pick) => pick.subject === person.subject);
      const hasDm =
        !group &&
        Boolean(
          existingDmPeers?.has(person.id) ||
            existingDmPeers?.has(person.subject.trim().toLowerCase()) ||
            existingDmPeers?.has(person.name.trim().toLowerCase()),
        );
      return {
        value: person.subject,
        label: person.name,
        hint: slug ? `@${slug}` : undefined,
        badge: alreadyPicked ? t("Added") : hasDm ? t("Existing DM") : undefined,
      };
    });
  }, [peopleList, group, selected, existingDmPeers, t]);

  const agentOptions = useMemo(
    () =>
      (agents ?? []).map((agent) => ({
        value: agent.uid,
        label: `@${agent.alias}`,
        badge: selectedAgents.some((pick) => pick.uid === agent.uid) ? t("Added") : undefined,
      })),
    [agents, selectedAgents, t],
  );

  const togglePerson = (subject: string) => {
    const person =
      peopleList.find((candidate) => candidate.subject === subject) ??
      knownPeopleRef.current.get(subject) ??
      selected.find((candidate) => candidate.subject === subject);
    if (!person) return;
    if (!group) {
      setSelected([person]);
      return;
    }
    setSelected((current) =>
      current.some((pick) => pick.subject === subject)
        ? current.filter((pick) => pick.subject !== subject)
        : [...current, person],
    );
  };

  const toggleAgent = (uid: string) => {
    const agent =
      agents?.find((candidate) => candidate.uid === uid) ??
      selectedAgents.find((candidate) => candidate.uid === uid);
    if (!agent) return;
    setSelectedAgents((current) =>
      current.some((pick) => pick.uid === uid)
        ? current.filter((pick) => pick.uid !== uid)
        : [...current, agent],
    );
  };

  const count = selected.length + selectedAgents.length;
  return (
    <form
      className="threads-create-form"
      aria-label={t(group ? "New channel" : "New direct message")}
      onSubmit={(event) => {
        event.preventDefault();
        if (pending || count === 0) return;
        setPending(true);
        onBusy(true);
        setError(null);
        const submit = async () => {
          if (!group) return openHumanDm(selected[0].subject);
          const input = {
            kind: selectedAgents.length > 0 ? ("mixed_group" as const) : ("human_group" as const),
            title: title.trim() || "Channel",
            humans: selected.map((person) => person.subject),
            agents: selectedAgents.map((agent) => agent.uid),
          };
          const key = JSON.stringify(input);
          if (envelope.current?.key !== key) envelope.current = { key, id: newMessageId() };
          return createHumanGroup({ ...input, client_id: envelope.current.id });
        };
        void submit()
          .then((room) => {
            envelope.current = null;
            onCreated(room);
          })
          .catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)))
          .finally(() => {
            setPending(false);
            onBusy(false);
          });
      }}
    >
      <div className="threads-create-body">
        <p className="threads-note">
          {t(
            group
              ? "Bring teammates together in a channel and optionally include shared agents."
              : "A private 1:1 conversation with one teammate in your workspace.",
          )}
        </p>
        {group && (
          <label className="threads-form-field" htmlFor={`${id}-title`}>
            <span className="threads-field-label">
              {t("Channel name")} <span className="threads-optional">{t("optional")}</span>
            </span>
            <input
              className="field"
              id={`${id}-title`}
              value={title}
              disabled={pending}
              maxLength={200}
              placeholder={t("e.g. Design review")}
              onChange={(event) => setTitle(event.target.value)}
            />
          </label>
        )}
        <div className="threads-form-field">
          <label className="threads-field-label" htmlFor={`${id}-people`}>
            {t(group ? "Teammates" : "Send to")}
          </label>
          {group && selected.length > 0 && (
            <div className="picked-chips threads-picked" aria-label={t("Selected teammates")}>
              {selected.map((person) => (
                <span className="threads-picked-person" key={person.subject}>
                  <span>{person.name}</span>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={pending}
                    icon={<IconClose size={12} />}
                    aria-label={`${t("Remove")} ${person.name}`}
                    onClick={() => setSelected((current) => current.filter((pick) => pick.subject !== person.subject))}
                  />
                </span>
              ))}
            </div>
          )}
          <Select
            id={`${id}-people`}
            full
            searchable
            keepOpenOnPick={group}
            selectedValues={group ? selected.map((person) => person.subject) : undefined}
            value={group ? "" : (selected[0]?.subject ?? "")}
            disabled={pending}
            loading={peopleLoading}
            emptyText={
              activePeople?.error
                ? t("Members unavailable")
                : term
                  ? t("No teammates match “{query}”.").replace("{query}", term)
                  : t("No other teammates in this workspace.")
            }
            placeholder={
              group
                ? selected.length > 0
                  ? t("Add more teammates…")
                  : t("Select teammates…")
                : t("Choose a teammate…")
            }
            aria-label={t(group ? "Teammates" : "Send to")}
            options={peopleOptions}
            onSearchChange={setPersonQuery}
            onChange={togglePerson}
          />
          {activePeople?.error && (
            <div className="threads-note flex items-center gap-2" role="alert">
              <span>{t("Members unavailable")}: {activePeople.error}</span>
              <Button variant="secondary" size="sm" disabled={pending} onClick={() => setPeopleRetry((v) => v + 1)}>
                {t("Retry search")}
              </Button>
            </div>
          )}
          {activePeople?.page?.has_more && (
            <p className="threads-note">{t("Showing first 20 matches. Type a name to narrow results.")}</p>
          )}
        </div>
        {group && (
          <div className="threads-form-field">
            <label className="threads-field-label" htmlFor={`${id}-agent`}>
              {t("Shared agents")} <span className="threads-optional">{t("optional")}</span>
            </label>
            {selectedAgents.length > 0 && (
              <div className="picked-chips threads-picked" aria-label={t("Selected agents")}>
                {selectedAgents.map((agent) => (
                  <span className="threads-picked-person" key={agent.uid}>
                    <span>@{agent.alias}</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={pending}
                      icon={<IconClose size={12} />}
                      aria-label={`${t("Remove")} @${agent.alias}`}
                      onClick={() => setSelectedAgents((current) => current.filter((pick) => pick.uid !== agent.uid))}
                    />
                  </span>
                ))}
              </div>
            )}
            <Select
              id={`${id}-agent`}
              full
              searchable
              keepOpenOnPick={false}
              selectedValues={selectedAgents.map((agent) => agent.uid)}
              value=""
              disabled={pending || agents === null}
              placeholder={
                agents === null
                  ? agentsError
                    ? t("Agents unavailable")
                    : t("Loading agents…")
                  : agents.length === 0
                    ? t("No agents available")
                    : selectedAgents.length > 0
                      ? t("Add more shared agents…")
                      : t("Select shared agents…")
              }
              aria-label={t("Shared agents")}
              options={agentOptions}
              onChange={toggleAgent}
            />
            {agentsError && (
              <p className="threads-note" role="status">
                {t("Agents unavailable")}: {agentsError}.
              </p>
            )}
          </div>
        )}
        <p className="threads-note">{t("Only channel members can read messages. You are included automatically.")}</p>
        {error && (
          <p className="threads-create-error" role="alert">
            {error}
          </p>
        )}
      </div>
      <div className="threads-create-footer">
        <span className="threads-note" role="status">
          {count ? `${count} ${t("selected")}` : t("Choose a member to continue")}
        </span>
        <div>
          <Button variant="secondary" type="button" onClick={onCancel} disabled={pending}>
            {t("Cancel")}
          </Button>
          <Button variant="primary" type="submit" disabled={pending || count === 0}>
            {pending ? t("Creating…") : t(group ? "Create channel" : "Start direct message")}
          </Button>
        </div>
      </div>
    </form>
  );
}
