import SafeLink from "../../ui/SafeLink";
import { useEffect, useRef, useState } from "react";
import { Resource, type ResourceState } from "../../lib/cache";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type {
  AgentsPayload,
  IssueDetail,
  IssueHistoryEntry,
} from "../../lib/types";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import SectionTabs from "../../ui/SectionTabs";
import "./issues.css";
import KickoffDialog from "./KickoffDialog";
import IdeaCard from "../home/IdeaCard";
import { LaneCard, type LanePayload } from "./LaneCard";
import { LaneConversation } from "./LaneConversation";
import { Activity } from "./Activity";
import { PrPanel } from "./PrPanel";
import { Evidence } from "./Evidence";
import { Fields } from "./Fields";
import { Links } from "./Links";
import { type IssuePageProps as Props } from "./issuePageProps";
import { useLocale } from "../../lib/locale";
import { ReadNotice } from "./issuePageShared";
import {
  acceptanceItems,
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

const STATUS_CHIP: Record<string, string> = {
  backlog: "bg-ink-800 text-ink-400",
  ready: "bg-ink-800 text-ink-200",
  doing: "bg-info/15 text-info",
  review: "bg-warn/15 text-warn",
  done: "bg-ok/15 text-ok",
  dropped: "bg-ink-800 text-ink-500",
};

export default function IssuePage(props: Props) {
  // All drafts and dialogs belong to one issue, including cached-to-cached navigation.
  return <IssueWorkspace key={props.id} {...props} />;
}

function IssueWorkspace(props: Props) {
  const state = useQuery(resources.issue(props.id));
  const laneState = useQuery(resources.lane(props.id));
  const detail = state.data?.id === props.id ? state.data : null;
  const lane = laneState.data?.issue === props.id ? laneState.data : null;
  const [historyResource] = useState(
    () =>
      new Resource(() => api.history(props.id, 40).then((r) => r.history), {
        isEmpty: (rows) => rows.length === 0,
      }),
  );
  const history = useQuery(historyResource);
  const revision = useRef(detail?.rev);
  const [kickoff, setKickoff] = useState(false);
  useEffect(() => {
    // History and detail settle independently, including the first cold revision.
    if (detail?.rev && detail.rev !== revision.current)
      void historyResource.invalidate();
    revision.current = detail?.rev;
  }, [detail?.rev, historyResource]);
  const dispatchBlock =
    props.kickoffBlock ??
    (props.readOnly ? (props.writeBlock ?? "Writes are off.") : null);
  const kickoffWhy = kickoffBlock(
    acceptanceItems(detail?.body ?? ""),
    dispatchBlock,
  );

  if (!detail) {
    return (
      <main className="px-4 lg:px-8 py-10" data-issue-page={props.id}>
        <h1 className="text-drawer font-semibold text-ink-100">{props.id}</h1>
        <ReadNotice
          name="issue"
          state={state}
          retry={() => void resources.issue(props.id).refresh()}
        />
      </main>
    );
  }

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
      {kickoff && (
        <KickoffDialog
          id={detail.id}
          title={detail.title}
          body={detail.body}
          blocked={kickoffWhy}
          groups={pmGroups(props.agents)}
          planHref={props.planHref}
          onClose={() => setKickoff(false)}
          onWrite={props.onWrite}
          onDone={props.onToast}
        />
      )}
    </>
  );
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
  onOpen,
  setKickoff,
}: Props & {
  detail: IssueDetail;
  history: ResourceState<IssueHistoryEntry[]>;
  issueState: ResourceState<IssueDetail>;
  laneState: ResourceState<LanePayload>;
  retryHistory: () => void;
  lane: LanePayload | null;
  blocked: string | null;
  setKickoff: (open: boolean) => void;
}) {
  const { t, formatNumber } = useLocale();
  const items = acceptanceItems(detail.body);
  const rows = timelineRows(detail, history.data ?? []);
  const pr = prRef(detail.refs);
  const done = items.filter((i) => i.checked).length;
  const fenced = laneFenceBanner(lane?.lane?.state);
  const images = detail.artifacts.filter((f) => IMG.test(f.name));
  const files = detail.artifacts.filter((f) => !IMG.test(f.name));
  const [compose, setCompose] = useState<"ask" | "instruct">("ask");
  const reloadLane = () => {
    void resources.issue(id).invalidate();
    void resources.lane(id).invalidate();
  };
  const epics = issues.filter(
    (i) =>
      i.project === project &&
      i.id !== id &&
      (i.container || i.work?.type === "epic"),
  );
  const hrefFor = (issueId: string) => {
    const card = issues.find((i) => i.id === issueId);
    return issuePath(card?.project ?? project, issueId);
  };

  const patch = (body: Parameters<typeof api.patch>[1], verb: string) => {
    api
      .patch(id, body, detail.rev)
      .then((r) => onWrite(r, verb))
      .catch((e) => onError(e, verb));
  };

  return (
    <div className="issue-workspace flex flex-col min-w-0" data-issue-page={id}>
      <header className="flex flex-wrap items-end justify-between gap-4 px-4 lg:px-8 pt-5 pb-3">
        <div className="min-w-0">
          <div className="kicker">
            <span className="num">{id}</span>
            {" · "}
            {project}
          </div>
          <h1 className="text-drawer font-semibold text-ink-100 tracking-tight mt-0.5">
            {detail.title}
          </h1>
          <div className="flex flex-wrap items-center gap-2 mt-2">
            <span
              className={`chip ${STATUS_CHIP[detail.status] ?? "bg-ink-800 text-ink-300"}`}
            >
              {t(detail.status)}
            </span>
            <span className="chip bg-ink-800 text-ink-400">
              {t(detail.priority)}
            </span>
            {(detail.parent || detail.component) && (
              <span className="chip bg-ink-800 text-ink-400">
                {detail.parent ?? detail.component}
              </span>
            )}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 ml-auto">
          <Button
            variant={blocked ? "secondary" : "primary"}
            disabled={!!blocked}
            title={blocked ?? undefined}
            onClick={() => setKickoff(true)}
          >
            {t("Kick off")}
          </Button>
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
      {(fenced || items.length === 0) && (
        <div className="grid gap-2 px-4 lg:px-8 pt-1">
          {fenced && (
            <div className="border border-ink-700 border-l-[3px] border-l-fail rounded-lg px-3.5 py-3 bg-fail/10">
              <strong className="text-ink-100">{t("Agent fenced")}</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">
                {t("A bound agent is fenced or needs attention. Unfence is on the lane card.")}
              </p>
            </div>
          )}
          {items.length === 0 && (
            <div className="border border-ink-700 border-l-[3px] border-l-warn rounded-lg px-3.5 py-3 bg-warn/10">
              <strong className="text-ink-100">{t("Kick off blocked")}</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">{blocked}</p>
            </div>
          )}
        </div>
      )}

      <SectionTabs
        label={t("Issue")}
        tabs={TABS.map((tabItem) => ({
          label: t(tabItem.label),
          href: tabHref(tabItem.id),
          on: tab === tabItem.id,
        }))}
      />

      <div className="grid xl:grid-cols-[minmax(0,1fr)_340px] items-start">
        <main className="min-w-0 px-4 lg:px-8 py-5 grid gap-4">
          {tab === "overview" && (detail.tags ?? []).includes("plan-ready") && (
            <section aria-label={t("Idea decision")}>
              <IdeaCard
                issue={id}
                readOnly={readOnly}
                onOpenIssue={onOpen}
                onDecided={() => void resources.issue(id).invalidate()}
              />
            </section>
          )}
          {tab === "overview" && (
            <section className="issue-description grid gap-4">
              {items.length > 0 && (
                <div className="issue-acceptance-progress">
                  <span>{t("Acceptance")}</span>
                  <strong className="num">
                    {formatNumber(done)} {t("of")} {formatNumber(items.length)} {t("complete")}
                  </strong>
                </div>
              )}
              <div className="issue-reader max-w-[84ch] text-secondary text-ink-300">
                <Md text={detail.body} onOpen={onOpen} />
              </div>
            </section>
          )}
          {tab === "activity" && (
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
            />
          )}
          {tab === "conversation" &&
            lane !== null &&
            (!laneState.error || lane.lane !== null) && (
              <LaneConversation
                issue={id}
                agent={lane?.lane?.agent ?? null}
                state={lane?.lane?.state ?? null}
                mode={compose}
                onMode={setCompose}
              />
            )}
          {tab === "pr" && (
            <PrPanel
              detail={detail}
              readOnly={readOnly}
              onWrite={onWrite}
              onError={onError}
            />
          )}
          {tab === "evidence" && (
            <Evidence
              id={id}
              images={images}
              files={files}
              readOnly={readOnly}
              onWrite={onWrite}
              onError={onError}
            />
          )}
        </main>
        <aside className="grid gap-3 px-4 lg:px-8 xl:px-4 xl:pr-8 py-5 xl:border-l xl:border-ink-700 min-w-0">
          <ReadNotice
            name="lane"
            state={laneState}
            retry={() => void resources.lane(id).refresh()}
          />
          {lane !== null && (!laneState.error || lane.lane !== null) && (
            <LaneCard
              issue={id}
              lane={lane?.lane ?? null}
              providers={lane?.providers ?? []}
              onCompose={(mode) => {
                setCompose(mode);
                navigate(tabHref("conversation"));
              }}
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
