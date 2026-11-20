import { useContext, useEffect, useState, type ReactNode } from "react";
import type { ResourceState } from "../../lib/cache";
import { issueIndex } from "../../lib/scope";
import { useTranslate } from "../../lib/locale";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconSearch } from "../../ui/icons";
import { resources } from "../../lib/resources";
import { readTeamMembers } from "../../lib/teamRoster";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import { HireBlock } from "../auth/WriteGate";
import AgentAvatar from "./AgentAvatar";
import SeatCard, { PersonalSeat, TeammateSeat } from "./SeatCard";
import MyAssistantPage from "./MyAssistantPage";
import { isCompanyMaster, masterShownTo, MASTER_LABEL, PERSONAL_ALIAS, personalOf } from "./personalAssistant";
import { MASTER } from "../home/master";
import { ROLES } from "../settings/ModelDefaults";
import RecruitChat from "./RecruitChat";
import type { ToastMsg } from "../../ui/Toast";
import AgentPage from "./AgentPage";
import HireSheet from "./HireSheet";
import ProfileSeat, { ConnectRuntime, workText } from "./ProfileSeat";
import AgentSidebar from "./AgentSidebar";
import NewAgentDialog from "./NewAgentDialog";
import Recruit from "./Recruit";
import { HireGate, RoleCard } from "./RecruitCards";
import {
  nextAlias,
  onTeamCount,
  roleOf,
  suggestedRoles,
  useAgentRoles,
} from "./agentRoles";
import type { AgentTab } from "./agentTabModel";
import {
  agentLocation,
  agentMatchesFilter,
  agentMatchesSearch,
  agentOrder,
  profileMatchesFilter,
  profileMatchesSearch,
  profileWork,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type { Agent, HiredProfile, AgentRole, AgentsPayload, IssueCard } from "../../lib/types";

/** "Accountant · this computer": the catalogue title, else "Master", else the
 *  team role's label (never the raw id or the provider), then where it runs. */
function roleLine(
  a: Agent,
  roleTitle: string | undefined,
  hosted: boolean,
  t: (s: string) => string,
  company = false,
): string {
  // CAD-2353: the company Master says who may use it, not where it runs.
  if (company) return t(MASTER_LABEL);
  const label = (id: string | null | undefined) => ROLES.find(([key]) => key === id)?.[1];
  const title =
    roleTitle ?? (a.alias === MASTER ? t("Master") : (label(a.team_role) ?? label(a.role) ?? t("Agent")));
  const where = agentLocation(a) === "cloud" ? t("cloud") : hosted ? t("workspace") : t("this computer");
  // The model a runtime resolved to; `inherit` is not a model, so it is not shown.
  const model = a.model && a.model !== "inherit" ? ` · ${a.model}` : "";
  return `${title} · ${where}${model}`;
}

export default function Agents({
  state,
  issues: issuesState,
  open,
  openProfileId,
  tab,
  recruit,
  ask,
  onAsk,
  onOpenAgent: setOpen,
  onTab,
  onToast,
  onRecruit,
  onOpenIssue,
  onRetry,
  operator,
  onRetryAccess,
  hostedBoard,
  viewerSub,
}: {
  /** `true` alone lets the viewer read and send agent conversations, as on
   *  Threads. `null` is an unanswered check, not a non-operator. */
  operator: boolean | null;
  onRetryAccess: () => void;
  hostedBoard: boolean;
  viewerSub?: string | null;
  state: ResourceState<AgentsPayload>;
  issues: ResourceState<IssueCard[]>;
  open: string | null;
  openProfileId?: string;
  tab: AgentTab;
  recruit: boolean;
  ask: boolean;
  onAsk: () => void;
  /** `tab` lands on a section instead of the chat (the company Master has no chat yet). */
  onOpenAgent: (alias: string | null, tab?: AgentTab, profileId?: string) => void;
  onTab: (tab: AgentTab) => void;
  onToast: (kind: ToastMsg["kind"], text: string) => void;
  onRecruit: (on: boolean) => void;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const t = useTranslate();
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState<AgentRole | null>(null);
  const [joined, setJoined] = useState<{ profile: HiredProfile; role: AgentRole } | null>(null);
  const roles = useAgentRoles();
  const installed = useQuery(resources.apps);
  const hireBlock = useContext(HireBlock);
  const payload = state.data;
  const issueIndexed = issueIndex(issuesState.data ?? []);
  // Agents belong to the team, not a project: the roster lists them all.
  // CAD-2353: the company Master is for owners; a hosted roster that names the
  // personal assistant does not list it to anyone else.
  const roster = payload?.agents ?? [];
  const allAgents = roster.filter((a) => masterShownTo(a, payload, hostedBoard, operator));
  const personal = personalOf(payload);
  const scopedRoster =
    hostedBoard &&
    ((payload != null && Object.hasOwn(payload, "personal_assistant")) ||
      roster.some((a) => isCompanyMaster(a, payload)));
  const directoryViewer = scopedRoster && !open && !openProfileId && !ask && !recruit ? viewerSub ?? null : null;
  const [directory, setDirectory] = useState<{
    viewer: string;
    rows: { userId: string; name: string }[];
  } | null>(null);
  useEffect(() => {
    if (!directoryViewer) {
      setDirectory(null);
      return;
    }
    // Every current workspace member has a personal seat. Show only the team
    // roster name; never read a teammate's profile or whether they opened Home.
    const abort = new AbortController();
    readTeamMembers(abort.signal)
      .then((members) => {
        if (abort.signal.aborted) return;
        setDirectory({
          viewer: directoryViewer,
          rows: members.flatMap(({ userId, name }) => {
            if (userId === directoryViewer) return [];
            const displayName = name.trim();
            return [{ userId, name: /^\S+@\S+$/.test(displayName) ? "" : displayName }];
          }),
        });
      })
      .catch(() => {
        if (!abort.signal.aborted) setDirectory(null);
      });
    return () => abort.abort();
  }, [directoryViewer]);
  const teammateRows = directory && directory.viewer === directoryViewer ? directory.rows : [];
  const searchTerm = query.trim().toLowerCase();
  const matchingTeammates = searchTerm
    ? teammateRows.filter(({ name }) => (name || t("Teammate")).toLowerCase().includes(searchTerm))
    : teammateRows;
  const companyMaster = allAgents.find((a) => isCompanyMaster(a, payload)) ?? null;
  const personalMatches =
    personal !== null &&
    `${personal.name} ${PERSONAL_ALIAS}`.toLowerCase().includes(searchTerm);
  const profiles = payload?.profiles ?? [];
  const idleProfiles = profiles.filter((p) => profileMatchesFilter(p, "all"));
  // The cards are the current team; a search reaches everyone. Stopped agents
  // and mailboxes otherwise live in the sidebar's folded groups.
  const searching = searchTerm !== "";
  const filter: AgentFilter = searching ? "all" : "current";
  const sidebarAgents = allAgents.filter((a) => agentMatchesSearch(a, query, issueIndexed));
  const sidebarProfiles = idleProfiles.filter((p) => profileMatchesSearch(p, query));
  const shownProfiles = idleProfiles.filter(
    (p) => profileMatchesFilter(p, filter) && profileMatchesSearch(p, query),
  );
  const agents = allAgents
    .filter((a) => agentMatchesFilter(a, filter) && agentMatchesSearch(a, query, issueIndexed))
    .sort(
      (a, b) =>
        Number(b.alias === MASTER) - Number(a.alias === MASTER) ||
        agentOrder(a) - agentOrder(b) ||
        a.alias.localeCompare(b.alias),
    );
  const emptyCopy =
    allAgents.length + idleProfiles.length + teammateRows.length === 0 && !personal
      ? t("No agents registered.")
      : searching
        ? t("No agents match “{query}”.").replace("{query}", () => query)
        : t("No current agents. Search to find stopped agents and mailboxes.");
  // A phone has no sidebar, so what it folds is listed under the cards there.
  const folded = searching ? [] : allAgents.filter((a) => !agentMatchesFilter(a, "current"));
  const agent = allAgents.find((a) => a.alias === open);
  const hosted = roles.data?.install.hosted ?? false;
  const suggestion = suggestedRoles(installed.data ?? [], roles.data?.roles ?? []);
  const groups = allAgents.filter((a) => a.role === "pm").map((a) => a.alias);
  const taken = [...allAgents.map((a) => a.alias), ...profiles.map((p) => p.alias)];
  const askMaster = onAsk;
  // Until the company Master chat exists, its row opens its page, not a chat.
  const openAgent = (alias: string) => {
    if (scopedRoster && alias === PERSONAL_ALIAS) {
      onToast("err", t("Agent details are unavailable."));
      return;
    }
    if (alias === MASTER && companyMaster && operator === true) setOpen(alias, "instructions");
    else setOpen(alias);
  };
  const openProfile = (profile: HiredProfile) => {
    if (scopedRoster) setOpen(null, undefined, profile.id);
    else openAgent(profile.alias);
  };
  const seat = (a: Agent) => (
    <SeatCard
      key={a.alias}
      agent={a}
      title={a === companyMaster ? (a.name ?? undefined) : undefined}
      roleLine={roleLine(a, roleOf(a, roles.data, profiles)?.title, hosted, t, a === companyMaster)}
      onOpen={() => openAgent(a.alias)}
    />
  );
  const profileTitle = (profile: HiredProfile) =>
    roles.data?.roles.find((r) => r.id === profile.catalogue)?.title ?? profile.role;
  const issueCards = issuesState.data ?? [];
  const hired = (profile: HiredProfile, role: AgentRole) => {
    setPicked(null);
    setJoined({ profile, role });
    onRecruit(false);
    onRetry();
  };
  const tile = (
    <button key="recruit-tile" type="button" className="rc-tile" onClick={() => onRecruit(true)}>
      <span className="plus" aria-hidden="true">＋</span>
      {t("Recruit a role")}
      <small>{t("Developer, Accountant, Support…")}</small>
    </button>
  );
  const sheet = picked && roles.data && (
    <HireSheet
      key={picked.id}
      role={picked}
      payload={roles.data}
      alias={nextAlias(picked.id, taken)}
      taken={taken}
      onClose={() => setPicked(null)}
      onHired={hired}
    />
  );

  const selectedProfile = scopedRoster && openProfileId ? profiles.find((p) => p.id === openProfileId) : undefined;
  const openAlias = selectedProfile?.alias ?? open;
  const openHired = selectedProfile ?? profiles.find((p) => p.alias === open);
  const shell = (content: ReactNode) => (
    <div className="ag-shell">
      <AgentSidebar
        agents={sidebarAgents}
        profiles={sidebarProfiles}
        personal={personalMatches ? personal : null}
        companyMaster={companyMaster}
        profileStatus={(p) => {
          const work = profileWork(p, issueCards);
          return work ? workText(work, t) : t("Idle · no runtime");
        }}
        open={open}
        openProfileId={openProfileId}
        query={query}
        hasTeammateMatches={matchingTeammates.length > 0}
        onQuery={setQuery}
        onOpen={openAgent}
        onOpenProfile={openProfile}
        onOpenPersonal={scopedRoster ? () => setOpen(PERSONAL_ALIAS) : null}
        onOverview={() => setOpen(null)}
        onRecruit={() => onRecruit(true)}
      />
      <div className="ag-shell-body">{content}</div>
    </div>
  );

  // Only a roster response can establish that a hosted alias is legacy.
  if (hostedBoard && payload == null && (open === PERSONAL_ALIAS || open === MASTER || openProfileId)) {
    return shell(
      <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
        <button type="button" className="rc-back" onClick={() => setOpen(null)}>
          ← {t("Agents")}
        </button>
        <ResourceGate state={state} loading="Loading agents…" failed="Could not load agents" onRetry={onRetry} />
      </main>,
    );
  }
  if (open === PERSONAL_ALIAS && personal) {
    return shell(<MyAssistantPage row={personal} onBack={() => setOpen(null)} onToast={onToast} rosterStale={state.status === "stale" || (payload != null && state.status === "failed")} />);
  }
  const detailUnavailable = (scopedRoster && open === PERSONAL_ALIAS) || (openProfileId && !selectedProfile);
  if (detailUnavailable || (scopedRoster && open === MASTER && operator !== true)) {
    return shell(
      <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
        <button type="button" className="rc-back" onClick={() => setOpen(null)}>
          ← {t("Agents")}
        </button>
        {detailUnavailable ? (
          <>
            <p className="rc-note" role="status">{t("Agent details are unavailable.")}</p>
            <Button onClick={onRetry}>{t("Retry")}</Button>
          </>
        ) : operator === null ? (
          <>
            <p className="rc-note">{t("We couldn’t confirm your access")}</p>
            <Button onClick={onRetryAccess}>{t("Retry")}</Button>
          </>
        ) : (
          <>
            <p className="rc-note">{t("Chat with the company Master is for owners. Your assistant is on Home.")}</p>
            <Button href="/">{t("Open Home")}</Button>
          </>
        )}
      </main>,
    );
  }
  if (openAlias) {
    return shell(
      <AgentPage
        key={selectedProfile?.id ?? openAlias}
        alias={openAlias}
        observed={selectedProfile ? undefined : agent}
        roles={roles}
        profiles={profiles}
        selectedProfile={selectedProfile}
        tab={tab}
        onTab={onTab}
        onBack={() => setOpen(null)}
        onOpenIssue={onOpenIssue}
        hosted={hosted}
        groups={groups}
        rosterLoaded={payload != null}
        rosterFailed={payload == null && state.status === "failed"}
        rosterStale={state.status === "stale" || (payload != null && state.status === "failed")}
        onRetryRoster={onRetry}
        operator={operator}
        onRetryAccess={onRetryAccess}
        hostedBoard={hostedBoard}
        scopedRoster={scopedRoster}
        companyMaster={open === MASTER && companyMaster !== null}
        hiredWork={openHired ? profileWork(openHired, issueCards) : null}
        onToast={onToast}
      />
    );
  }
  if (ask) {
    return shell(
      <RecruitChat
        showDecisions={scopedRoster && operator === true}
        hireBlock={hireBlock}
        onBack={() => setOpen(null)}
      />
    );
  }
  if (recruit) {
    return shell(
      <>
        <Recruit
          roles={roles}
          profiles={profiles}
          hireBlock={hireBlock}
          onBack={() => onRecruit(false)}
          onPick={setPicked}
          onAskMaster={askMaster}
          onBlank={() => setCreating(true)}
        />
        {sheet}
        {creating && (
          <NewAgentDialog
            onClose={() => setCreating(false)}
            onCreated={() => {
              onRetry();
              onRecruit(false);
            }}
          />
        )}
      </>
    );
  }

  return shell(
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <div className="rc-head">
        <div>
          <h1>
            {t("Agents")} <StaleChip state={state} />
          </h1>
          <p>{t("Your team. Recruit a ready-made role, or ask Master to find one.")}</p>
        </div>
        <div className="rc-acts">
          <Button onClick={askMaster}>{t("Ask Master")}</Button>
          <Button
            variant="primary"
            onClick={() => onRecruit(true)}
            title={hireBlock ?? t("Recruit an agent from a ready-made role")}
          >
            ＋ {t("Recruit")}
          </Button>
        </div>
      </div>
      <HireGate reason={hireBlock} />
      {joined && profiles.some((p) => p.id === joined.profile.id && p.runtimes.length === 0) && (
        <div className="rc-nudge" role="status">
          <AgentAvatar slug="custom" look={joined.role.look} size={48} still />
          <div>
            <div className="t">
              {t("{name} was added as {role}")
                .replace("{name}", () => joined.profile.alias)
                .replace("{role}", () => joined.role.title)}
            </div>
            <div className="s">{t("Idle · no runtime")}</div>
            <ConnectRuntime profile={joined.profile} hosted={hosted} groups={groups} />
          </div>
        </div>
      )}
      {payload?.daemon === "unreachable" && (
        <div className="card mb-4 px-4 py-3 text-label text-warn border-warn/40" role="status">
          {t("Daemon unavailable. Showing the last known agent state.")}
        </div>
      )}
      <ResourceGate state={state} loading="Loading agents…" failed="Could not load agents" onRetry={onRetry} />

      {payload && (
        <>
          <div className="ag-toolbar" role="search" aria-label={t("Find agents")}>
            <label className="ag-search" htmlFor="agents-search">
              <IconSearch />
              <span className="sr-only">{t("Search agents")}</span>
              <input
                id="agents-search"
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("Search agents, work, or issues")}
              />
            </label>
          </div>
          <div className="rc-seats">
            {personalMatches && personal && <PersonalSeat row={personal} onOpen={() => setOpen(PERSONAL_ALIAS)} />}
            {agents.map(seat)}
            {shownProfiles.map((profile) => (
              <ProfileSeat
                key={`profile-${profile.id}`}
                profile={profile}
                roleTitle={profileTitle(profile)}
                work={profileWork(profile, issueCards)}
                onOpen={() => openProfile(profile)}
              />
            ))}
            {!searching && tile}
          </div>
          {matchingTeammates.length > 0 && (
            <section aria-labelledby="teammate-assistants-heading">
              <div className="rc-sec">
                <h2 id="teammate-assistants-heading">{t("Teammate assistants")}</h2>
              </div>
              <ul className="rc-seats" role="list">
                {matchingTeammates.map(({ userId, name }) => <TeammateSeat key={userId} name={name} />)}
              </ul>
              <p className="mt-2 text-label text-ink-500">{t("Only the teammate can open their personal assistant.")}</p>
            </section>
          )}
          {folded.length > 0 && (
            <details className="ag-folded">
              <summary>
                {t("Stopped and mailboxes")} <span className="num">{folded.length}</span>
              </summary>
              <div className="rc-seats">
                {folded.map(seat)}
              </div>
            </details>
          )}
          {agents.length + shownProfiles.length + matchingTeammates.length === 0 && !personalMatches && (
            <p className="text-label text-ink-500 mt-3" role="status">
              {emptyCopy}
            </p>
          )}
          {roles.data && suggestion.roles.length > 0 && (
            <>
              <div className="rc-sec">
                <h2>{t("Suggested for you")}</h2>
                <span>{t("based on your apps: {apps}").replace("{apps}", () => suggestion.labels.join(", "))}</span>
              </div>
              <div className="rc-grid">
                {suggestion.roles.map((role) => (
                  <RoleCard
                    key={role.id}
                    role={role}
                    payload={roles.data!}
                    onTeam={onTeamCount(role, profiles)}
                    onPick={setPicked}
                  />
                ))}
              </div>
            </>
          )}
        </>
      )}
      {sheet}
      {creating && <NewAgentDialog onClose={() => setCreating(false)} onCreated={() => onRetry()} />}
    </main>
  );
}
