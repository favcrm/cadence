import { useEffect, useRef, useState } from "react";
import { Resource, type ResourceState } from "../../lib/cache";
import { api, type WriteResp } from "../../lib/api";
import { fmtBytes, fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type {
  AgentsPayload,
  IssueCard,
  IssueDetail,
  IssueHistoryEntry,
} from "../../lib/types";
import { navigate } from "../../lib/useLocation";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import SectionTabs from "../../ui/SectionTabs";
import { IconClose } from "../../ui/icons";
import "./issues.css";
import { noDragReason } from "../projects/Card";
import KickoffDialog from "./KickoffDialog";
import { LaneCard, type LanePayload } from "./LaneCard";
import { LaneConversation } from "./LaneConversation";
import {
  acceptanceItems,
  issuePath,
  kickoffBlock,
  laneFenceBanner,
  prRef,
  shownLinks,
  timelineRows,
  type IssueTab,
} from "./model";

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];
const LINK_KINDS = [
  { value: "blocked_by", label: "Blocked by" },
  { value: "relates", label: "Related to" },
  { value: "parent", label: "Parent" },
  { value: "duplicate_of", label: "Duplicate of" },
];
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

const TONE: Record<string, string> = {
  done: "bg-ok",
  now: "bg-accent",
  warn: "bg-warn",
  wait: "bg-ink-600",
};

interface Props {
  project: string;
  id: string;
  tab: IssueTab;
  tabHref: (tab: IssueTab) => string;
  issues: IssueCard[];
  agents: AgentsPayload | null;
  readOnly: boolean;
  writeBlock: string | null;
  kickoffBlock: string | null;
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onOpen: (id: string) => void;
  planHref: string;
  onToast: (text: string) => void;
}

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
    // The mount read already covers the first revision; later writes refresh history.
    if (detail?.rev && revision.current && detail.rev !== revision.current)
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

function ReadNotice<T>({
  name,
  state,
  retry,
}: {
  name: string;
  state: ResourceState<T>;
  retry: () => void;
}) {
  if (!state.error && !state.inFlight && state.status !== "loading")
    return null;
  const loading = state.inFlight || state.status === "loading";
  return (
    <div
      className="issue-read-notice"
      data-read-state={name}
      role={state.error ? "alert" : "status"}
    >
      <div>
        {state.error ? (
          <>
            <strong>
              {name[0].toUpperCase() + name.slice(1)} could not be loaded.
            </strong>
            <p>
              {state.error}
              {state.data !== null
                ? " Showing the last known information."
                : ""}
            </p>
          </>
        ) : (
          <span>
            {state.data === null ? "Loading" : "Refreshing"} {name}…
          </span>
        )}
      </div>
      {state.error && (
        <Button loading={loading} onClick={retry}>
          Retry {name}
        </Button>
      )}
    </div>
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
              {detail.status}
            </span>
            <span className="chip bg-ink-800 text-ink-400">
              {detail.priority}
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
            Kick off
          </Button>
          {lane?.lane && (
            <Button href={tabHref("conversation")}>Ask agent</Button>
          )}
          {pr?.href && (
            <a className="btn" href={pr.href} target="_blank" rel="noreferrer">
              Open pull request
            </a>
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
              <strong className="text-ink-100">Agent fenced</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">
                A bound agent is fenced or needs attention. Unfence is on the
                lane card.
              </p>
            </div>
          )}
          {items.length === 0 && (
            <div className="border border-ink-700 border-l-[3px] border-l-warn rounded-lg px-3.5 py-3 bg-warn/10">
              <strong className="text-ink-100">Kick off blocked</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">{blocked}</p>
            </div>
          )}
        </div>
      )}

      <SectionTabs
        label="Issue"
        tabs={TABS.map((t) => ({
          label: t.label,
          href: tabHref(t.id),
          on: tab === t.id,
        }))}
      />

      <div className="grid xl:grid-cols-[minmax(0,1fr)_340px] items-start">
        <main className="min-w-0 px-4 lg:px-8 py-5 grid gap-4">
          {tab === "overview" && (
            <section className="issue-description grid gap-4">
              {items.length > 0 && (
                <div className="issue-acceptance-progress">
                  <span>Acceptance</span>
                  <strong className="num">
                    {done} of {items.length} complete
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

function Activity({
  id,
  rows,
  history,
  retryHistory,
  readOnly,
  rev,
  onWrite,
  onError,
  onOpen,
}: {
  id: string;
  rows: ReturnType<typeof timelineRows>;
  history: ResourceState<IssueHistoryEntry[]>;
  retryHistory: () => void;
  readOnly: boolean;
  rev: string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
  onOpen: (id: string) => void;
}) {
  const [comment, setComment] = useState("");
  const [preview, setPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);
  const send = async () => {
    const body = comment.trim();
    if (!body || readOnly || pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await api.comment(id, body, rev);
      onWrite(result, `${id} comment`);
      setComment("");
      setPreview(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Comment could not be posted.");
      onError(e, "comment");
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  return (
    <section className="grid gap-4">
      <ReadNotice name="history" state={history} retry={retryHistory} />
      {rows.length === 0 && history.data !== null && !history.error ? (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            No activity yet
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            Comments and changes to this issue will appear here.
          </p>
        </div>
      ) : rows.length > 0 ? (
        <>
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            Recent activity
          </h2>
          <ol className="grid issue-timeline">
            {rows.map((row, i) => (
              <li key={`${row.title}-${i}`} className="relative pl-5 pb-4">
                <i
                  className={`absolute left-0 top-1.5 w-2 h-2 rounded-full ${TONE[row.tone]}`}
                />
                {i < rows.length - 1 && (
                  <i className="absolute left-[3px] top-4 bottom-0 w-px bg-ink-700" />
                )}
                <div className="flex items-baseline justify-between gap-3">
                  <b className="text-ink-100 font-semibold">{row.title}</b>
                  {row.at && (
                    <span className="num text-micro text-ink-500">
                      {fmtTime(row.at)}
                    </span>
                  )}
                </div>
                <div className="issue-reader text-secondary text-ink-400 mt-0.5">
                  {row.markdown ? (
                    <Md text={row.detail} onOpen={onOpen} />
                  ) : (
                    row.detail
                  )}
                </div>
              </li>
            ))}
          </ol>
        </>
      ) : null}
      {!readOnly && (
        <div className="card p-4 grid gap-2.5">
          <div className="flex items-baseline gap-2">
            <label className="slabel" htmlFor="issue-comment">
              Comment
            </label>
            <Button
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={() => setPreview((v) => !v)}
            >
              {preview ? "Edit comment" : "Preview"}
            </Button>
          </div>
          {preview ? (
            <div className="issue-reader card p-3 text-secondary text-ink-300 min-h-16">
              {comment.trim() ? (
                <Md text={comment} onOpen={onOpen} />
              ) : (
                <span className="text-ink-500">
                  Write a comment to preview it.
                </span>
              )}
            </div>
          ) : (
            <textarea
              id="issue-comment"
              disabled={busy}
              className="field w-full !h-auto py-2 text-secondary"
              rows={3}
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="Add an update or a question…"
            />
          )}
          {error && (
            <p className="text-secondary text-fail m-0" role="alert">
              {error}
            </p>
          )}
          <div className="flex">
            <Button
              variant="primary"
              loading={busy}
              disabled={!comment.trim()}
              onClick={send}
            >
              {busy ? "Posting…" : "Post comment"}
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}

function PrPanel({
  detail,
  readOnly,
  onWrite,
  onError,
}: {
  detail: IssueDetail;
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const pr = prRef(detail.refs);
  const reports = detail.reports ?? [];
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  return (
    <div className="grid gap-5">
      <section className="card p-4 grid gap-3">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
          {pr?.label ?? "No pull request yet"}
        </h2>
        {pr ? (
          <>
            <p className="text-secondary text-ink-400 m-0">
              View live checks and merge queue status on the pull request.
            </p>
            {pr.href ? (
              <a
                className="lnk text-secondary break-all"
                href={pr.href}
                target="_blank"
                rel="noreferrer"
              >
                {pr.href}
              </a>
            ) : (
              <p className="text-secondary text-ink-400 m-0">
                This reference has no URL.
              </p>
            )}
          </>
        ) : (
          <>
            <p className="text-secondary text-ink-400 m-0">
              Link a pull request to keep its reviews and delivery evidence
              together.
            </p>
            {!readOnly && (
              <form
                className="issue-pr-form"
                onSubmit={async (e) => {
                  e.preventDefault();
                  const target = url.trim();
                  if (!target || pending.current || readOnly) return;
                  pending.current = true;
                  setBusy(true);
                  try {
                    const result = await api.addRef(
                      detail.id,
                      "pr",
                      { url: target },
                      undefined,
                      detail.rev,
                    );
                    onWrite(result, `${detail.id} ref pr`);
                    setUrl("");
                  } catch (e) {
                    onError(e, "ref");
                  } finally {
                    pending.current = false;
                    setBusy(false);
                  }
                }}
              >
                <label className="grid gap-1.5 min-w-0">
                  <span className="slabel">Pull request URL</span>
                  <input
                    className="field w-full min-w-0"
                    type="url"
                    required
                    disabled={busy}
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    placeholder="https://github.com/…/pull/1"
                  />
                </label>
                <Button type="submit" loading={busy} disabled={!url.trim()}>
                  Add PR
                </Button>
              </form>
            )}
          </>
        )}
      </section>
      <section className="grid gap-3">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
          Review reports
        </h2>
        {reports.length === 0 ? (
          <p className="text-secondary text-ink-400 m-0">
            No review reports recorded on this issue.
          </p>
        ) : (
          reports.map((r) => (
            <article key={r.name} className="card p-4 grid gap-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <b className="text-ink-100">{r.agent ?? r.name}</b>
                <span className="chip bg-ink-800 text-ink-300">
                  {r.kind ?? "report"}
                </span>
              </div>
              {r.at && (
                <time className="num text-micro text-ink-500" dateTime={r.at}>
                  {fmtTime(r.at)}
                </time>
              )}
              {r.body && (
                <div className="issue-reader">
                  <Md text={r.body} />
                </div>
              )}
            </article>
          ))
        )}
      </section>
    </div>
  );
}

function Attach({
  id,
  onWrite,
  onError,
}: {
  id: string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <Button onClick={() => input.current?.click()}>Attach</Button>
      <input
        ref={input}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => {
          const list = e.target.files;
          if (!list) return;
          for (const f of Array.from(list)) {
            api
              .attach(id, f.name, f)
              .then((r) => onWrite(r, `${id} attach ${f.name}`))
              .catch((err) => onError(err, `attach ${f.name}`));
          }
          e.target.value = "";
        }}
      />
    </>
  );
}

function Evidence({
  id,
  images,
  files,
  readOnly,
  onWrite,
  onError,
}: {
  id: string;
  images: { name: string; size: number }[];
  files: { name: string; size: number }[];
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const empty = images.length === 0 && files.length === 0;
  return (
    <section className="grid gap-4">
      {empty && (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            No evidence yet
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            Add screenshots, test results, and other supporting files.
          </p>
        </div>
      )}
      {images.length > 0 && (
        <div className="grid gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            Screenshots
          </h2>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {images.map((f) => (
              <figure key={f.name} className="card overflow-hidden m-0">
                <a
                  href={api.artifactUrl(id, f.name)}
                  target="_blank"
                  rel="noreferrer"
                >
                  <img
                    src={api.artifactUrl(id, f.name)}
                    alt={f.name}
                    className="w-full h-28 object-cover bg-ink-900"
                  />
                </a>
                <figcaption className="flex justify-between gap-2 px-2.5 py-2 text-label">
                  <span className="num truncate" title={f.name}>
                    {f.name}
                  </span>
                  <span className="text-ink-500 shrink-0">
                    {fmtBytes(f.size)}
                  </span>
                </figcaption>
              </figure>
            ))}
          </div>
        </div>
      )}
      {files.length > 0 && (
        <div className="card">
          <div className="slabel px-3 pt-2.5">Artifacts</div>
          <ul>
            {files.map((f) => (
              <li
                key={f.name}
                className="flex items-center gap-3 px-3 py-2 border-t border-ink-800"
              >
                <a
                  className="lnk num text-label truncate"
                  href={api.artifactUrl(id, f.name)}
                  target="_blank"
                  rel="noreferrer"
                  title={f.name}
                >
                  {f.name}
                </a>
                <span className="num text-micro text-ink-500 ml-auto shrink-0">
                  {fmtBytes(f.size)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {!readOnly && <Attach id={id} onWrite={onWrite} onError={onError} />}
    </section>
  );
}

function Fields({
  detail,
  epics,
  readOnly,
  onWrite,
  onError,
  onPatch,
}: {
  detail: IssueDetail;
  epics: IssueCard[];
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
  onPatch: (body: Parameters<typeof api.patch>[1], verb: string) => void;
}) {
  const locked = noDragReason(detail);
  const [owner, setOwner] = useState(detail.owner ?? "");
  useEffect(() => setOwner(detail.owner ?? ""), [detail.owner, detail.rev]);
  const setEpic = (next: string) => {
    const prev = detail.parent;
    const chain = prev
      ? api.unlink(detail.id, "parent", prev, detail.rev).then((r) => {
          if (!next) return r;
          return api.link(detail.id, "parent", next, r.issue.rev);
        })
      : next
        ? api.link(detail.id, "parent", next, detail.rev)
        : Promise.resolve(null);
    chain
      .then((r) => {
        if (r) onWrite(r, `${detail.id} epic`);
      })
      .catch((e) => onError(e, "epic"));
  };
  return (
    <section className="card p-3.5 grid gap-2.5" aria-label="Fields">
      <div className="slabel">Fields</div>
      <label className="grid gap-1.5">
        <span className="slabel">Status</span>
        <Select
          full
          value={detail.status}
          disabled={readOnly || !!locked}
          title={locked ?? undefined}
          aria-label="Status"
          options={[
            ...(STATUSES.includes(detail.status)
              ? []
              : [{ value: detail.status, label: detail.status }]),
            ...STATUSES.map((s) => ({ value: s, label: s })),
          ]}
          onChange={(status) => onPatch({ status }, `${detail.id} status`)}
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">Priority</span>
        <Select
          full
          value={detail.priority}
          disabled={readOnly}
          aria-label="Priority"
          options={[
            ...(PRIORITIES.includes(detail.priority)
              ? []
              : [{ value: detail.priority, label: detail.priority }]),
            ...PRIORITIES.map((p) => ({ value: p, label: p })),
          ]}
          onChange={(priority) =>
            onPatch({ priority }, `${detail.id} priority`)
          }
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">Owner</span>
        <input
          className="field w-full"
          value={owner}
          disabled={readOnly}
          aria-label="Owner"
          onChange={(e) => setOwner(e.target.value)}
          onBlur={() => {
            if ((detail.owner ?? "") !== owner)
              onPatch({ owner }, `${detail.id} owner`);
          }}
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">Epic</span>
        <Select
          full
          value={detail.parent ?? ""}
          disabled={readOnly}
          aria-label="Epic"
          options={[
            { value: "", label: "None" },
            ...(detail.parent && !epics.some((e) => e.id === detail.parent)
              ? [{ value: detail.parent, label: detail.parent }]
              : []),
            ...epics.map((e) => ({
              value: e.id,
              label: `${e.id} · ${e.title}`,
            })),
          ]}
          onChange={setEpic}
        />
      </label>
    </section>
  );
}

function Links({
  detail,
  readOnly,
  hrefFor,
  onWrite,
  onError,
}: {
  detail: IssueDetail;
  readOnly: boolean;
  hrefFor: (id: string) => string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const [kind, setKind] = useState("relates");
  const [target, setTarget] = useState("");
  const rows = shownLinks(detail.links);
  const references = detail.refs.filter(
    (r) => r.kind === "note" || r.kind === "url",
  );
  const unlink = (unlinkKind: string, targetId: string) => {
    api
      .unlink(detail.id, unlinkKind, targetId, detail.rev)
      .then((r) => onWrite(r, `${detail.id} unlink ${unlinkKind} ${targetId}`))
      .catch((err) => onError(err, "unlink"));
  };
  return (
    <section className="card p-3.5 grid gap-2" aria-label="Links">
      <div className="slabel">Links</div>
      {rows.length === 0 && references.length === 0 && (
        <p className="text-secondary text-ink-500 m-0">No links yet.</p>
      )}
      <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
        {rows.map((row) => (
          <span key={`${row.label}-${row.id}`} className="contents">
            <dt className="slabel">{row.label}</dt>
            <dd className="text-secondary min-w-0 flex items-center gap-1.5">
              <span className="min-w-0 truncate">
                {row.missing ? (
                  <span className="num text-ink-500">{row.id}</span>
                ) : (
                  <Link className="lnk num" href={hrefFor(row.id)}>
                    {row.id}
                  </Link>
                )}
                {row.title ? (
                  <span className="text-ink-400"> · {row.title}</span>
                ) : null}
              </span>
              {row.unlinkKind && !readOnly && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="ml-auto shrink-0"
                  aria-label={`Unlink ${row.label} ${row.id}`}
                  title="Remove this link"
                  onClick={() => unlink(row.unlinkKind!, row.id)}
                >
                  <IconClose />
                </Button>
              )}
            </dd>
          </span>
        ))}
        {references.map((r, i) => (
          <span key={`reference-${i}`} className="contents">
            <dt className="slabel">
              {r.kind === "note" ? "Note" : "Reference"}
            </dt>
            <dd
              className="text-secondary min-w-0 truncate"
              title={r.url ?? r.path ?? r.label}
            >
              {r.url ? (
                <a
                  className="lnk"
                  href={r.url}
                  target="_blank"
                  rel="noreferrer"
                >
                  {r.label ?? r.url}
                </a>
              ) : (
                <span>{r.label ?? r.path}</span>
              )}
            </dd>
          </span>
        ))}
      </dl>
      {!readOnly && (
        <form
          className="issue-link-form"
          onSubmit={(e) => {
            e.preventDefault();
            const t = target.trim();
            if (!t) return;
            api
              .link(detail.id, kind, t, detail.rev)
              .then((r) => {
                onWrite(r, `${detail.id} ${kind} ${t}`);
                setTarget("");
              })
              .catch((err) => onError(err, "link"));
          }}
        >
          <Select
            value={kind}
            aria-label="link type"
            options={LINK_KINDS}
            onChange={setKind}
          />
          <input
            className="field flex-1 min-w-0 num text-label"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="CAD-16"
            aria-label="link target"
          />
          <Button type="submit" disabled={!target.trim()}>
            Link
          </Button>
        </form>
      )}
    </section>
  );
}
