import SafeLink from "../../ui/SafeLink";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { type ResourceState } from "../../lib/cache";
import { api, ApiError } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useMaybeResource, useQuery } from "../../lib/useResource";
import type {
  AgentsPayload,
  IssueDetail,
  IssueHistoryEntry,
} from "../../lib/types";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import SectionTabs from "../../ui/SectionTabs";
import { buildMentionCandidates } from "../mentions/candidates";
import { useWorkspaceTeammates } from "../mentions/useWorkspacePeople";
import "./issues.css";
import KickoffDialog from "./KickoffDialog";
import HostedDispatchDialog from "./HostedDispatchDialog";
import { selectCloudSpecialists } from "./hostedDispatchModel";
import IdeaCard from "../home/IdeaCard";
import { stageLabel } from "../projects/work";
import { LaneCard, type LanePayload } from "./LaneCard";
import { LaneConversation } from "./LaneConversation";
import { Activity } from "./Activity";
import { PrPanel } from "./PrPanel";
import { Evidence } from "./Evidence";
import { Fields } from "./Fields";
import { PRIORITY_CHIP, STATUS_CHIP } from "./chips";
import { WorkResult } from "./WorkResult";
import { Links } from "./Links";
import { type IssuePageProps as Props } from "./issuePageProps";
import { useLocale } from "../../lib/locale";
import { ReadNotice } from "./issuePageShared";
import { IssuePageSkeleton, LaneCardSkeleton } from "./IssueSkeleton";
import {
  acceptanceItems,
  hasCodeDelivery,
  issuePath,
  kickoffBlock,
  laneFenceBanner,
  prRef,
  timelineRows,
  type IssueTab,
} from "./model";

const IMG = /\.(png|jpe?g|gif|webp)$/i;

const TABS: { id: IssueTab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "activity", label: "Activity" },
  { id: "conversation", label: "Conversation" },
  { id: "pr", label: "Pull request" },
  { id: "evidence", label: "Evidence" },
];

const EMPTY_HISTORY_STATE: ResourceState<IssueHistoryEntry[]> = {
  data: null,
  status: "loading",
  error: null,
  asOf: null,
  inFlight: false,
};

export default function IssuePage(props: Props) {
  // All drafts and dialogs belong to one issue, including cached-to-cached navigation.
  return <IssueWorkspace key={props.id} {...props} />;
}

function IssueWorkspace(props: Props) {
  const state = useQuery(resources.issue(props.id));
  const laneResource = props.hosted ? null : resources.lane(props.id);
  useEffect(() => {
    if (laneResource) void laneResource.revalidate();
  }, [laneResource]);
  const laneState = useMaybeResource(laneResource);
  const detail = state.data?.id === props.id ? state.data : null;
  const lane = laneState?.data?.issue === props.id ? laneState.data : null;
  const wantHistory = props.tab === "activity";
  const historyResource = resources.issueHistory(props.id);
  useEffect(() => {
    if (wantHistory) void historyResource.revalidate();
  }, [wantHistory, historyResource]);
  const history = useMaybeResource(wantHistory ? historyResource : null) ?? EMPTY_HISTORY_STATE;
  const revision = useRef(detail?.rev);
  const [kickoff, setKickoff] = useState(false);
  useEffect(() => {
    // History and detail settle independently, including the first cold revision.
    if (detail?.rev && detail.rev !== revision.current) {
      if (wantHistory) {
        const snap = historyResource.get();
        if (revision.current !== undefined || snap.asOf !== null) {
          void historyResource.invalidate();
        }
      } else if (revision.current !== undefined) {
        historyResource.markInvalid();
      }
    }
    revision.current = detail?.rev;
  }, [detail?.rev, wantHistory, historyResource]);
  const dispatchBlock =
    props.kickoffBlock ??
    (props.readOnly ? (props.writeBlock ?? "Writes are off.") : null);
  const kickoffWhy = kickoffBlock(
    acceptanceItems(detail?.body ?? ""),
    dispatchBlock,
  );

  if (!detail) {
    if (state.error) {
      return (
        <main className="px-4 lg:px-8 py-6 grid gap-4" data-issue-page={props.id}>
          <div>
            <div className="kicker">
              <span className="num normal-case">{props.id}</span>
              {" · "}
              <Link
                className="hover:text-ink-200 transition-colors"
                href={`/projects/${encodeURIComponent(props.project)}/issues`}
              >
                {props.project}
              </Link>
            </div>
            <h1 className="text-drawer font-semibold text-ink-100 tracking-tight mt-0.5">
              {props.id}
            </h1>
          </div>
          <ReadNotice
            name="issue"
            state={state}
            retry={() => void resources.issue(props.id).refresh()}
          />
        </main>
      );
    }
    const seedCard = props.issues.find((item) => item.id === props.id) ?? null;
    return (
      <IssuePageSkeleton
        id={props.id}
        project={props.project}
        card={seedCard}
        hosted={props.hosted === true}
        tab={props.tab}
      />
    );
  }

  const cloudProfiles = selectCloudSpecialists(props.agents?.profiles);
  const canHostedDispatch = Boolean(
    props.hosted && !props.readOnly && (props.canLocalDispatch || cloudProfiles.length > 0),
  );

  return (
    <>
      <PageBody
        {...props}
        detail={detail}
        history={history}
        lane={lane}
        issueState={state}
        laneState={laneState}
        retryHistory={() => void historyResource.refresh()}
        blocked={kickoffWhy}
        setKickoff={setKickoff}
      />
      {kickoff && canHostedDispatch && (
        <HostedDispatchDialog
          id={detail.id}
          title={detail.title}
          rev={detail.rev}
          currentOwner={detail.owner}
          canLocalDispatch={props.canLocalDispatch === true}
          cloudProfiles={cloudProfiles}
          daemonsHref={props.daemonsHref}
          onClose={() => setKickoff(false)}
          onWrite={props.onWrite}
          onDispatched={() => void resources.issue(detail.id).invalidate()}
          onToast={props.onToast}
        />
      )}
      {kickoff && !props.hosted && (
        <KickoffDialog
          id={detail.id}
          title={detail.title}
          body={detail.body}
          blocked={kickoffWhy}
          groups={pmGroups(props.agents)}
          seats={seatOptions(props.agents)}
          planHref={props.planHref}
          onClose={() => setKickoff(false)}
          onWrite={props.onWrite}
          onDone={props.onToast}
        />
      )}
    </>
  );
}

function seatOptions(
  agents: AgentsPayload | null,
): { alias: string; provider: string; state: string | null }[] {
  return (agents?.agents ?? [])
    .filter((a) => a.provider !== "inbox" && !a.dead)
    .map((a) => ({ alias: a.alias, provider: a.provider, state: a.state }));
}

function pmGroups(agents: AgentsPayload | null): string[] {
  const roots = (agents?.agents ?? [])
    .filter((a) => a.group_root)
    .map((a) => a.alias);
  return roots.length ? roots : ["master"];
}

function PageBody({
  project,
  id,
  tab,
  tabHref,
  issues,
  agents,
  detail,
  history,
  lane,
  issueState,
  laneState,
  retryHistory,
  readOnly,
  blocked,
  onWrite,
  onError,
  onToast,
  onOpen,
  setKickoff,
  hosted,
  canLocalDispatch,
  managedFiles,
  projectRepos,
  stagedActions,
  viewerSub,
}: Props & {
  detail: IssueDetail;
  history: ResourceState<IssueHistoryEntry[]>;
  issueState: ResourceState<IssueDetail>;
  laneState: ResourceState<LanePayload> | null;
  retryHistory: () => void;
  lane: LanePayload | null;
  blocked: string | null;
  setKickoff: (open: boolean) => void;
}) {
  const { t } = useLocale();
  const fieldsId = useId();
  const teammates = useWorkspaceTeammates(hosted === true, viewerSub);
  const items = useMemo(() => acceptanceItems(detail.body), [detail.body]);
  const pr = prRef(detail.refs);
  const codeDelivery = hasCodeDelivery(detail, projectRepos, hosted);
  const visibleTabs = TABS.filter((tabItem) => {
    if (tabItem.id === "pr" && !codeDelivery) return false;
    if (tabItem.id === "conversation" && hosted && !lane?.lane) return false;
    return true;
  });
  const activeTab: IssueTab = visibleTabs.some((tabItem) => tabItem.id === tab)
    ? tab
    : "overview";
  const rows = useMemo(
    () => (activeTab === "activity" ? timelineRows(detail, history.data ?? []) : []),
    [activeTab, detail, history.data],
  );
  const done = items.filter((i) => i.checked).length;
  const fenced = laneFenceBanner(lane?.lane?.state);
  const images = useMemo(() => detail.artifacts.filter((f) => IMG.test(f.name)), [detail.artifacts]);
  const files = useMemo(() => detail.artifacts.filter((f) => !IMG.test(f.name)), [detail.artifacts]);
  const [compose, setCompose] = useState<"ask" | "instruct">("ask");
  const [copied, setCopied] = useState(false);
  const copyTimer = useRef<number | null>(null);
  useEffect(() => () => {
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
  }, []);
  const copyId = () => {
    if (!navigator.clipboard?.writeText) return;
    void navigator.clipboard.writeText(id).then(() => {
      setCopied(true);
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(false), 1500);
    }, () => undefined);
  };
  const laneLoading =
    !hosted &&
    lane === null &&
    laneState !== null &&
    !laneState.error &&
    (laneState.inFlight || laneState.status === "loading");
  const [descriptionDraft, setDescriptionDraft] = useState<{ body: string; rev: string } | null>(null);
  const editingDescription = hosted === true && !readOnly && descriptionDraft !== null;
  const [savingDescription, setSavingDescription] = useState(false);
  const [descriptionConflict, setDescriptionConflict] = useState(false);
  const descriptionStale = descriptionConflict || (descriptionDraft !== null && descriptionDraft.rev !== detail.rev);
  const reloadLane = () => {
    void resources.issue(id).invalidate();
    if (!hosted) void resources.lane(id).invalidate();
  };
  const epics = useMemo(
    () =>
      issues.filter(
        (i) =>
          i.project === project &&
          i.id !== id &&
          (i.container || i.work?.type === "epic"),
      ),
    [issues, project, id],
  );
  const hrefFor = (issueId: string) => {
    const card = issues.find((i) => i.id === issueId);
    return issuePath(card?.project ?? project, issueId);
  };

  const composeWith = (mode: "ask" | "instruct") => {
    setCompose(mode);
    navigate(tabHref("conversation"));
  };

  const patch = (body: Parameters<typeof api.patch>[1], verb: string) =>
    api
      .patch(id, body, detail.rev)
      .then((r) => onWrite(r, verb))
      .catch((e) => onError(e, verb));
  const saveDescription = async () => {
    if (!descriptionDraft || descriptionStale || savingDescription || readOnly) return;
    setSavingDescription(true);
    try {
      const result = await api.patch(id, { body: descriptionDraft.body }, descriptionDraft.rev);
      onWrite(result, t("Description"));
      setDescriptionDraft(null);
    } catch (error) {
      if (error instanceof ApiError && error.status === 409) {
        setDescriptionConflict(true);
        void resources.issue(id).invalidate();
      } else {
        onError(error, t("Description"));
      }
    } finally {
      setSavingDescription(false);
    }
  };

  return (
    <div className="issue-workspace flex flex-col min-w-0" data-issue-page={id}>
      <header className="flex flex-wrap items-end justify-between gap-4 px-4 lg:px-8 pt-5 pb-3">
        <div className="min-w-0">
          <div className="kicker flex items-center gap-1 flex-wrap">
            <button
              type="button"
              className="issue-kicker-copy num normal-case"
              onClick={copyId}
              aria-label={t("Copy issue ID")}
              title={t("Copy issue ID")}
            >
              <span>{id}</span>
              {copied && <span className="text-micro text-ok">{t("Copied")}</span>}
            </button>
            <span aria-hidden="true">·</span>
            <Link
              className="hover:text-ink-200 transition-colors"
              href={`/projects/${encodeURIComponent(project)}/issues`}
            >
              {project}
            </Link>
          </div>
          <h1 className="issue-title font-semibold text-ink-100 tracking-tight mt-0.5">
            {detail.title}
          </h1>
          <div className="flex flex-wrap items-center gap-2 mt-2">
            <span
              className={`chip ${STATUS_CHIP[detail.status] ?? "bg-ink-800 text-ink-300"}`}
            >
              {t(detail.status)}
            </span>
            <span
              className={`chip ${PRIORITY_CHIP[detail.priority] ?? "bg-ink-800 text-ink-400"}`}
            >
              {t(detail.priority)}
            </span>
            {detail.work?.stage && (
              <span className="text-label text-ink-400">{stageLabel(detail.work.stage, t)}</span>
            )}
            {detail.component && (
              <span className="text-label text-ink-400">{detail.component}</span>
            )}
            <a className="lnk text-label xl:hidden" href={`#${fieldsId}`}>
              {t("Fields & links")}
            </a>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 sm:ml-auto">
          {hosted && !readOnly && (canLocalDispatch || selectCloudSpecialists(agents?.profiles).length > 0) && (
            <Button variant="primary" onClick={() => setKickoff(true)}>
              {t("Dispatch")}
            </Button>
          )}
          {!hosted && (
            <Button
              variant={blocked ? "secondary" : "primary"}
              disabled={!!blocked}
              title={blocked == null ? undefined : t(blocked)}
              onClick={() => setKickoff(true)}
            >
              {t("Kick off")}
            </Button>
          )}
          {lane?.lane && (
            <Button href={tabHref("conversation")}>{t("Ask agent")}</Button>
          )}
          {pr?.href && (
            <SafeLink className="btn" warnClassName="btn" href={pr.href}>
              {t("Open pull request")}
            </SafeLink>
          )}
        </div>
      </header>

      <div className="px-4 lg:px-8">
        <ReadNotice
          name="issue"
          state={issueState}
          retry={() => void resources.issue(id).refresh()}
        />
      </div>
      {(fenced || (!hosted && items.length === 0)) && (
        <div className="grid gap-2 px-4 lg:px-8 pt-1">
          {fenced && (
            <div className="border border-ink-700 border-l-[3px] border-l-fail rounded-lg px-3.5 py-3 bg-fail/10">
              <strong className="text-ink-100">{t("Agent fenced")}</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">
                {t("A bound agent is fenced or needs attention. Unfence is on the lane card.")}
              </p>
            </div>
          )}
          {!hosted && items.length === 0 && (
            <div className="border border-ink-700 border-l-[3px] border-l-warn rounded-lg px-3.5 py-3 bg-warn/10">
              <strong className="text-ink-100">{t("Kick off blocked")}</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">{blocked && t(blocked)}</p>
            </div>
          )}
        </div>
      )}

      <SectionTabs
        label={t("Issue")}
        tabs={visibleTabs.map((tabItem) => ({
          label: t(tabItem.label),
          href: tabHref(tabItem.id),
          on: activeTab === tabItem.id,
        }))}
      />

      <div className="grid grid-cols-[minmax(0,1fr)] xl:grid-cols-[minmax(0,1fr)_340px] items-start">
        <main className="min-w-0 px-4 lg:px-8 py-5 grid grid-cols-[minmax(0,1fr)] gap-5">
          {activeTab === "overview" && (detail.tags ?? []).includes("plan-ready") && (
            <section aria-label={t("Idea decision")}>
              <IdeaCard
                issue={id}
                readOnly={readOnly}
                onOpenIssue={onOpen}
                onDecided={() => void resources.issue(id).invalidate()}
              />
            </section>
          )}
          {activeTab === "overview" && (
            <WorkResult
              detail={detail}
              lane={lane}
              laneLoading={laneLoading}
              checksDone={done}
              checksTotal={items.length}
              checklistVisible={!editingDescription}
              blocked={blocked}
              hosted={hosted === true}
              canLocalDispatch={canLocalDispatch === true}
              readOnly={readOnly}
              codeDelivery={codeDelivery}
              stagedActions={stagedActions}
              tabHref={tabHref}
              onCompose={composeWith}
              onAssignmentMutated={() => void resources.issue(id).invalidate()}
              onToast={onToast}
              onOpen={onOpen}
            >
              {editingDescription ? (
                <form className="grid gap-2" onSubmit={(event) => {
                  event.preventDefault();
                  void saveDescription();
                }}>
                  <label className="slabel" htmlFor="issue-description-draft">{t("Description")}</label>
                  <textarea
                    id="issue-description-draft"
                    name="body"
                    className="field w-full !h-auto py-2"
                    rows={10}
                    value={descriptionDraft.body}
                    readOnly={savingDescription}
                    onChange={(event) => setDescriptionDraft({ ...descriptionDraft, body: event.target.value })}
                    autoFocus
                  />
                  {descriptionStale && (
                    <p role="alert" className="text-secondary text-warn">
                      {t("The issue changed while you were editing. Copy your draft, then reopen the editor to use the latest version.")}
                    </p>
                  )}
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit" variant="primary" loading={savingDescription} disabled={descriptionStale}>
                      {t("Save changes")}
                    </Button>
                    <Button disabled={savingDescription} onClick={() => setDescriptionDraft(null)}>{t("Cancel")}</Button>
                  </div>
                </form>
              ) : (
                <>
                  {hosted && !readOnly && detail.rev && (
                    <div>
                      <Button size="sm" onClick={() => {
                        setDescriptionConflict(false);
                        setDescriptionDraft({ body: detail.body, rev: detail.rev });
                      }}>
                        {t("Edit description")}
                      </Button>
                    </div>
                  )}
                  <Md text={detail.body} onOpen={onOpen} />
                </>
              )}
            </WorkResult>
          )}
          {activeTab === "activity" && (
            <Activity
              id={id}
              history={history}
              retryHistory={retryHistory}
              rows={rows}
              readOnly={readOnly}
              rev={detail.rev}
              onWrite={onWrite}
              onError={onError}
              onOpen={onOpen}
              mentionCandidates={buildMentionCandidates(
                agents?.agents,
                teammates,
                t("Teammate · notify"),
                undefined,
                agents?.profiles,
              )}
            />
          )}
          {activeTab === "conversation" &&
            lane !== null &&
            (!laneState?.error || lane.lane !== null) && (
              <LaneConversation
                issue={id}
                agent={lane?.lane?.agent ?? null}
                state={lane?.lane?.state ?? null}
                mode={compose}
                onMode={setCompose}
              />
            )}
          {activeTab === "pr" && (
            <PrPanel
              detail={detail}
              readOnly={readOnly}
              onWrite={onWrite}
              onError={onError}
            />
          )}
          {activeTab === "evidence" && (
            <Evidence
              id={id}
              images={images}
              files={files}
              readOnly={readOnly}
              managedFiles={managedFiles}
              onWrite={onWrite}
              onError={onError}
            />
          )}
        </main>
        <aside id={fieldsId} tabIndex={-1} className="issue-context grid grid-cols-[minmax(0,1fr)] gap-5 px-4 lg:px-8 xl:px-6 xl:pr-8 py-5 min-w-0">
          {!hosted && laneState && (
            laneLoading ? (
              <LaneCardSkeleton />
            ) : (
              <ReadNotice
                name="lane"
                state={laneState}
                retry={() => void resources.lane(id).refresh()}
              />
            )
          )}
          {!hosted && lane !== null && (!laneState?.error || lane.lane !== null) && (
            <LaneCard
              issue={id}
              lane={lane?.lane ?? null}
              providers={lane?.providers ?? []}
              onCompose={composeWith}
              onChanged={reloadLane}
            />
          )}
          <Fields
            detail={detail}
            epics={epics}
            readOnly={readOnly}
            onWrite={onWrite}
            onError={onError}
            onPatch={patch}
          />
          <Links
            detail={detail}
            readOnly={readOnly}
            hrefFor={hrefFor}
            onWrite={onWrite}
            onError={onError}
          />
        </aside>
      </div>
    </div>
  );
}
