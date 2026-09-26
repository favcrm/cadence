import { useEffect, useState } from "react";
import { api, type WriteResp } from "../../lib/api";
import { fmtBytes, fmtTime } from "../../lib/fmt";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { AgentsPayload, IssueCard, IssueDetail, IssueHistoryEntry } from "../../lib/types";
import Md from "../../ui/Md";
import Link from "../../ui/Link";
import { noDragReason } from "../projects/Card";
import ConversationSlot from "./Conversation";
import KickoffDialog from "./KickoffDialog";
import LaneCard from "./LaneCard";
import {
  acceptanceItems,
  approveReason,
  askAgentReason,
  CI_UNAVAILABLE,
  issuePath,
  kickoffBlock,
  prRef,
  QUEUE_UNAVAILABLE,
  timelineRows,
  type IssueTab,
} from "./model";

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];
const LINK_KINDS = ["blocked_by", "relates", "parent", "duplicate_of"];
const IMG = /\.(png|jpe?g|gif|webp)$/i;

const TABS: { id: IssueTab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "activity", label: "Activity" },
  { id: "conversation", label: "Conversation" },
  { id: "pr", label: "PR & CI" },
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

const btn =
  "h-8 px-3 inline-flex items-center rounded border border-ink-600 text-label text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-40 disabled:cursor-not-allowed";
const btnAcc =
  "h-8 px-3 inline-flex items-center rounded bg-accent text-on-accent text-label font-medium hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed";

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
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onOpen: (id: string) => void;
  planHref: string;
  onToast: (text: string) => void;
}

export default function IssuePage(props: Props) {
  const state = useQuery(resources.issue(props.id));
  const detail = state.data?.id === props.id ? state.data : null;
  const [history, setHistory] = useState<IssueHistoryEntry[]>([]);
  const [kickoff, setKickoff] = useState(false);

  useEffect(() => {
    let live = true;
    api
      .history(props.id, 40)
      .then((r) => live && setHistory(r.history))
      .catch(() => live && setHistory([]));
    return () => {
      live = false;
    };
  }, [props.id, detail?.rev]);

  if (!detail) {
    return (
      <main className="px-4 lg:px-8 py-10" data-issue-page={props.id}>
        <h1 className="text-drawer font-semibold text-ink-100">{props.id}</h1>
        <p className="text-secondary text-ink-400 mt-2">
          {state.status === "failed" ? state.error ?? "Could not load this issue." : "Loading…"}
        </p>
      </main>
    );
  }

  return (
    <>
      <PageBody {...props} detail={detail} history={history} setKickoff={setKickoff} />
      {kickoff && (
        <KickoffDialog
          id={detail.id}
          title={detail.title}
          body={detail.body}
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
  const roots = (agents?.agents ?? []).filter((a) => a.group_root).map((a) => a.alias);
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
  readOnly,
  writeBlock,
  onWrite,
  onError,
  onOpen,
  setKickoff,
}: Props & {
  detail: IssueDetail;
  history: IssueHistoryEntry[];
  setKickoff: (open: boolean) => void;
}) {
  const items = acceptanceItems(detail.body);
  const blocked = kickoffBlock(items, readOnly ? writeBlock ?? "Writes are off." : null);
  const agents = detail.agents ?? [];
  const askWhy = askAgentReason(agents.length);
  const approveWhy = approveReason(detail.refs);
  const rows = timelineRows(detail, history);
  const done = items.filter((i) => i.checked).length;
  const fenced = agents.some((a) => a.state === "fenced" || a.state === "attention");
  const images = detail.artifacts.filter((f) => IMG.test(f.name));
  const files = detail.artifacts.filter((f) => !IMG.test(f.name));
  const epics = issues.filter((i) => i.project === project && i.id !== id && (i.container || i.work?.type === "epic"));
  const hrefFor = (issueId: string) => {
    const card = issues.find((i) => i.id === issueId);
    return issuePath(card?.project ?? project, issueId);
  };

  const patch = (body: Parameters<typeof api.patch>[1], verb: string) => {
    api.patch(id, body, detail.rev).then((r) => onWrite(r, verb)).catch((e) => onError(e, verb));
  };

  return (
    <div className="flex flex-col min-w-0" data-issue-page={id}>
      <header className="flex flex-wrap items-end justify-between gap-4 px-4 lg:px-8 pt-5 pb-3">
        <div className="min-w-0">
          <div className="kicker">
            <span className="num">{id}</span>
            {" · "}
            {project}
          </div>
          <h1 className="text-drawer font-semibold text-ink-100 tracking-tight mt-0.5">{detail.title}</h1>
          <div className="flex flex-wrap items-center gap-2 mt-2">
            <span className={`chip ${STATUS_CHIP[detail.status] ?? "bg-ink-800 text-ink-300"}`}>{detail.status}</span>
            <span className="chip bg-ink-800 text-ink-400">{detail.priority}</span>
            {(detail.parent || detail.component) && (
              <span className="chip bg-ink-800 text-ink-400">{detail.parent ?? detail.component}</span>
            )}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2 ml-auto">
          <button type="button" className={blocked ? btn : btnAcc} disabled={!!blocked} title={blocked ?? undefined} onClick={() => setKickoff(true)}>
            Kick off
          </button>
          {askWhy ? (
            <button type="button" className={btn} disabled title={askWhy}>Ask agent</button>
          ) : (
            <Link className={btn} href={tabHref("conversation")}>Ask agent</Link>
          )}
          <button type="button" className={btn} disabled title={approveWhy}>Approve</button>
        </div>
      </header>

      {(fenced || items.length === 0) && (
        <div className="grid gap-2 px-4 lg:px-8 pt-1">
          {fenced && (
            <div className="border border-ink-700 border-l-[3px] border-l-fail rounded-lg px-3.5 py-3 bg-fail/10">
              <strong className="text-ink-100">Agent fenced</strong>
              <p className="text-secondary text-ink-300 mt-1 m-0">
                A bound agent is fenced or needs attention. Unfence stays on the lane card.
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

      <nav className="px-4 lg:px-8 mt-3 border-b border-ink-700 flex gap-1 overflow-x-auto" aria-label="Issue sections" role="tablist">
        {TABS.map((t) => (
          <Link
            key={t.id}
            href={tabHref(t.id)}
            replace
            role="tab"
            aria-current={tab === t.id ? "page" : undefined}
            className={`h-9 inline-flex items-center px-2.5 text-label border-b-2 -mb-px whitespace-nowrap ${
              tab === t.id ? "border-accent text-accent font-medium" : "border-transparent text-ink-400 hover:text-ink-100"
            }`}
          >
            {t.label}
          </Link>
        ))}
      </nav>

      <div className="grid xl:grid-cols-[minmax(0,1fr)_300px] items-start">
        <main className="min-w-0 px-4 lg:px-8 py-5 grid gap-4">
          {tab === "overview" && (
            <>
              <div className="max-w-[74ch] text-secondary text-ink-300">
                <Md text={detail.body} onOpen={onOpen} />
              </div>
              <section className="grid gap-2.5">
                <div className="flex items-baseline justify-between gap-3">
                  <h2 className="text-cardtitle font-semibold text-ink-100 m-0">Acceptance</h2>
                  {items.length > 0 && <span className="num text-micro text-ink-500">{done} of {items.length}</span>}
                </div>
                {items.length === 0 ? (
                  <div className="card px-4 py-8 text-center">
                    <div className="text-cardtitle font-semibold text-ink-100">No acceptance yet</div>
                    <p className="text-secondary text-ink-400 mt-1">Kick off needs at least one check. Add the checklist, then dispatch.</p>
                  </div>
                ) : (
                  <ul className="card px-3.5">
                    {items.map((item) => (
                      <li key={item.text} className="flex gap-2 py-2 border-b border-ink-800 last:border-0 text-secondary text-ink-200">
                        <input type="checkbox" checked={item.checked} readOnly aria-label={item.text} className="mt-1 accent-accent" />
                        <span>{item.text}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            </>
          )}
          {tab === "activity" && (
            <Activity id={id} rows={rows} readOnly={readOnly} rev={detail.rev} onWrite={onWrite} onError={onError} onOpen={onOpen} />
          )}
          {tab === "conversation" && <ConversationSlot issueId={id} agentAlias={agents[0]?.alias ?? null} />}
          {tab === "pr" && <PrPanel detail={detail} readOnly={readOnly} onWrite={onWrite} onError={onError} />}
          {tab === "evidence" && (
            <Evidence id={id} images={images} files={files} readOnly={readOnly} onWrite={onWrite} onError={onError} />
          )}
        </main>
        <aside className="grid gap-3 px-4 lg:px-8 xl:px-4 xl:pr-8 py-5 xl:border-l xl:border-ink-700 min-w-0">
          <LaneCard
            agents={agents}
            refs={detail.refs}
            activity={agents[0]?.message ?? null}
            kickoffBlocked={blocked}
            onKickoff={() => setKickoff(true)}
            askHref={askWhy ? null : tabHref("conversation")}
          />
          <Fields detail={detail} epics={epics} readOnly={readOnly} onWrite={onWrite} onError={onError} onPatch={patch} />
          <Links detail={detail} readOnly={readOnly} hrefFor={hrefFor} onWrite={onWrite} onError={onError} />
        </aside>
      </div>
    </div>
  );
}

function Activity({
  id,
  rows,
  readOnly,
  rev,
  onWrite,
  onError,
  onOpen,
}: {
  id: string;
  rows: ReturnType<typeof timelineRows>;
  readOnly: boolean;
  rev: string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
  onOpen: (id: string) => void;
}) {
  const [comment, setComment] = useState("");
  const [preview, setPreview] = useState(false);
  const send = () => {
    const body = comment.trim();
    if (!body) return;
    api
      .comment(id, body, rev)
      .then((r) => {
        onWrite(r, `${id} comment`);
        setComment("");
        setPreview(false);
      })
      .catch((e) => onError(e, "comment"));
  };
  return (
    <section className="grid gap-4 max-w-[68ch]">
      {rows.length === 0 ? (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">No activity yet</div>
          <p className="text-secondary text-ink-400 mt-1">The timeline starts when a lane is claimed.</p>
        </div>
      ) : (
        <>
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">Timeline</h2>
          <ol className="grid">
            {rows.map((row, i) => (
              <li key={`${row.title}-${i}`} className="relative pl-5 pb-4">
                <i className={`absolute left-0 top-1.5 w-2 h-2 rounded-full ${TONE[row.tone]}`} />
                {i < rows.length - 1 && <i className="absolute left-[3px] top-4 bottom-0 w-px bg-ink-700" />}
                <div className="flex items-baseline justify-between gap-3">
                  <b className="text-ink-100 font-semibold">{row.title}</b>
                  {row.at && <span className="num text-micro text-ink-500">{fmtTime(row.at)}</span>}
                </div>
                <div className="text-secondary text-ink-400 mt-0.5">
                  {row.markdown ? <Md text={row.detail} onOpen={onOpen} /> : row.detail}
                </div>
              </li>
            ))}
          </ol>
        </>
      )}
      {!readOnly && (
        <div className="grid gap-1.5">
          <div className="flex items-baseline gap-2">
            <span className="slabel">Comment</span>
            <button type="button" className="lnk num text-micro" onClick={() => setPreview((v) => !v)}>
              {preview ? "write" : "preview"}
            </button>
          </div>
          {preview ? (
            <div className="card p-3 text-secondary text-ink-300 min-h-16">
              {comment.trim() ? <Md text={comment} onOpen={onOpen} /> : <span className="text-ink-500">nothing to preview</span>}
            </div>
          ) : (
            <textarea
              className="field w-full !h-auto py-2 text-secondary"
              rows={3}
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="markdown — raw html stays inert"
            />
          )}
          <button type="button" className={`${btn} justify-self-start`} disabled={!comment.trim()} onClick={send}>Comment</button>
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
  if (!pr) {
    return (
      <div className="grid gap-3">
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">No pull request yet</div>
          <p className="text-secondary text-ink-400 mt-1">Checks, verdicts, and the merge queue show up after a PR exists.</p>
        </div>
        {!readOnly && (
          <form
            className="flex gap-1.5"
            onSubmit={(e) => {
              e.preventDefault();
              const t = url.trim();
              if (!t) return;
              api
                .addRef(detail.id, "pr", { url: t }, undefined, detail.rev)
                .then((r) => {
                  onWrite(r, `${detail.id} ref pr`);
                  setUrl("");
                })
                .catch((err) => onError(err, "ref"));
            }}
          >
            <input className="field flex-1" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://…/pull/1" aria-label="pull request url" />
            <button type="submit" className={btn} disabled={!url.trim()}>Add PR</button>
          </form>
        )}
      </div>
    );
  }
  return (
    <div className="grid gap-4">
      <div>
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">{pr.label}</h2>
        {pr.href && <a className="lnk text-secondary" href={pr.href} target="_blank" rel="noreferrer">{pr.href}</a>}
      </div>
      <div className="card p-3.5 grid gap-2">
        <div className="slabel">Checks</div>
        <p className="text-secondary text-ink-400 m-0">{CI_UNAVAILABLE}</p>
      </div>
      <section className="grid gap-2">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">Verdicts</h2>
        {reports.length === 0 ? (
          <p className="text-secondary text-ink-400 m-0">No verdicts on this issue yet.</p>
        ) : (
          reports.map((r) => (
            <article key={r.name} className="card p-3.5 grid gap-1.5">
              <div className="flex items-center justify-between gap-2">
                <b className="text-ink-100">{r.agent ?? r.name}</b>
                <span className="chip bg-ink-800 text-ink-300">{r.kind ?? "report"}</span>
              </div>
              {r.body ? <Md text={r.body} /> : null}
            </article>
          ))
        )}
      </section>
      <div className="card p-3.5 grid gap-1.5">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">Merge queue</h2>
        <p className="text-secondary text-ink-400 m-0">
          {QUEUE_UNAVAILABLE} Approve records a human-class decision once a board route exists. It does not merge by itself.
        </p>
      </div>
    </div>
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
          <div className="text-cardtitle font-semibold text-ink-100">No evidence yet</div>
          <p className="text-secondary text-ink-400 mt-1">Screenshots and artifacts land here once a lane is working.</p>
        </div>
      )}
      {images.length > 0 && (
        <div className="grid gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">Screenshots</h2>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {images.map((f) => (
              <figure key={f.name} className="card overflow-hidden m-0">
                <a href={api.artifactUrl(id, f.name)} target="_blank" rel="noreferrer">
                  <img src={api.artifactUrl(id, f.name)} alt={f.name} className="w-full h-28 object-cover bg-ink-900" />
                </a>
                <figcaption className="flex justify-between gap-2 px-2.5 py-2 text-label">
                  <span className="num truncate" title={f.name}>{f.name}</span>
                  <span className="text-ink-500 shrink-0">{fmtBytes(f.size)}</span>
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
              <li key={f.name} className="flex items-center gap-3 px-3 py-2 border-t border-ink-800">
                <a className="lnk num text-label truncate" href={api.artifactUrl(id, f.name)} target="_blank" rel="noreferrer" title={f.name}>{f.name}</a>
                <span className="num text-micro text-ink-500 ml-auto shrink-0">{fmtBytes(f.size)}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {!readOnly && (
        <label className={`${btn} justify-self-start cursor-pointer`}>
          Attach
          <input
            type="file"
            multiple
            className="hidden"
            onChange={(e) => {
              const list = e.target.files;
              if (!list) return;
              for (const f of Array.from(list)) {
                api.attach(id, f.name, f).then((r) => onWrite(r, `${id} attach ${f.name}`)).catch((err) => onError(err, `attach ${f.name}`));
              }
              e.target.value = "";
            }}
          />
        </label>
      )}
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
        <select
          className="field w-full"
          value={detail.status}
          disabled={readOnly || !!locked}
          title={locked ?? undefined}
          aria-label="Status"
          onChange={(e) => onPatch({ status: e.target.value }, `${detail.id} status`)}
        >
          {STATUSES.includes(detail.status) ? null : <option value={detail.status}>{detail.status}</option>}
          {STATUSES.map((s) => <option key={s}>{s}</option>)}
        </select>
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">Priority</span>
        <select
          className="field w-full"
          value={detail.priority}
          disabled={readOnly}
          aria-label="Priority"
          onChange={(e) => onPatch({ priority: e.target.value }, `${detail.id} priority`)}
        >
          {PRIORITIES.includes(detail.priority) ? null : <option>{detail.priority}</option>}
          {PRIORITIES.map((p) => <option key={p}>{p}</option>)}
        </select>
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
            if ((detail.owner ?? "") !== owner) onPatch({ owner }, `${detail.id} owner`);
          }}
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">Epic</span>
        <select className="field w-full" value={detail.parent ?? ""} disabled={readOnly} aria-label="Epic" onChange={(e) => setEpic(e.target.value)}>
          <option value="">None</option>
          {detail.parent && !epics.some((e) => e.id === detail.parent) && <option value={detail.parent}>{detail.parent}</option>}
          {epics.map((e) => <option key={e.id} value={e.id}>{e.id} · {e.title}</option>)}
        </select>
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
  const rows: { label: string; id: string; title?: string; missing: boolean }[] = [];
  const l = detail.links;
  if (l.parent) rows.push({ label: "Parent", id: l.parent.id, title: l.parent.title, missing: l.parent.missing });
  for (const b of l.blocks) rows.push({ label: "Blocks", id: b.id, title: b.title, missing: b.missing });
  for (const b of l.blocked_by) rows.push({ label: "Blocked by", id: b.id, title: b.title, missing: b.missing });
  for (const r of l.relates) rows.push({ label: "Related", id: r.id, title: r.title, missing: r.missing });
  const wiki = detail.refs.filter((r) => r.kind === "note" || r.kind === "url");
  return (
    <section className="card p-3.5 grid gap-2" aria-label="Links">
      <div className="slabel">Links</div>
      {rows.length === 0 && wiki.length === 0 && <p className="text-secondary text-ink-500 m-0">No links yet.</p>}
      <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
        {rows.map((row) => (
          <span key={`${row.label}-${row.id}`} className="contents">
            <dt className="slabel">{row.label}</dt>
            <dd className="text-secondary min-w-0">
              {row.missing ? (
                <span className="num text-ink-500">{row.id}</span>
              ) : (
                <Link className="lnk num" href={hrefFor(row.id)}>{row.id}</Link>
              )}
              {row.title ? <span className="text-ink-400"> · {row.title}</span> : null}
            </dd>
          </span>
        ))}
        {wiki.map((r, i) => (
          <span key={`wiki-${i}`} className="contents">
            <dt className="slabel">Wiki</dt>
            <dd className="text-secondary min-w-0 truncate" title={r.url ?? r.path ?? r.label}>
              {r.url ? (
                <a className="lnk" href={r.url} target="_blank" rel="noreferrer">{r.label ?? r.url}</a>
              ) : (
                <span>{r.label ?? r.path}</span>
              )}
            </dd>
          </span>
        ))}
      </dl>
      {!readOnly && (
        <form
          className="flex gap-1.5"
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
          <select className="field !h-8 text-label" value={kind} onChange={(e) => setKind(e.target.value)} aria-label="link type">
            {LINK_KINDS.map((k) => <option key={k}>{k}</option>)}
          </select>
          <input className="field !h-8 flex-1 min-w-0 num text-label" value={target} onChange={(e) => setTarget(e.target.value)} placeholder="CAD-16" aria-label="link target" />
          <button type="submit" className={btn} disabled={!target.trim()}>Link</button>
        </form>
      )}
    </section>
  );
}
