import { useEffect, useRef, useState } from "react";
import { api, type WriteResp } from "../api";
import { fmtBytes, fmtTime } from "../fmt";
import Md from "./Md";
import { noDragReason } from "./Card";
import type {
  AgentsPayload,
  IssueDetail,
  IssueHistoryEntry,
  LinkRef,
  Project,
} from "../types";

const NOTE_CHIP: Record<string, string> = {
  kickoff: "bg-info/10 text-info",
  qa: "bg-warn/10 text-warn",
  verdict: "bg-ok/10 text-ok",
};

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];
const LINK_KINDS = ["blocked_by", "relates", "parent", "duplicate_of"];
const REF_KINDS = ["pr", "commit", "note", "preview", "message", "url"];
const IMG_EXT = /\.(png|jpe?g|gif|webp)$/i;

interface Props {
  id: string;
  agents: AgentsPayload | null;
  projects: Project[];
  pmDir?: string;
  detail: IssueDetail | null;
  onClose: () => void;
  onOpen: (id: string) => void;
  onWrite: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
}

/// `/home/ubuntu/pm/cadence/CAD-16/issue.md` → `~/pm/cadence/CAD-16/issue.md`
function relPath(path: string, pmDir?: string): string {
  if (pmDir && path.startsWith(pmDir + "/")) {
    return `~/pm/${path.slice(pmDir.length + 1)}`;
  }
  return path;
}

function LinkRow({
  label,
  link,
  onOpen,
  onRemove,
}: {
  label: string;
  link: LinkRef;
  onOpen: (id: string) => void;
  onRemove?: () => void;
}) {
  return (
    <li className="flex items-center gap-2.5 px-3 py-2.5">
      <span className="chip bg-ink-800 text-ink-400 w-[5.4rem] justify-center shrink-0 whitespace-nowrap">
        {label}
      </span>
      {link.missing ? (
        <span className="num text-label text-ink-500 shrink-0 whitespace-nowrap">
          {link.id}
        </span>
      ) : (
        <button
          onClick={() => onOpen(link.id)}
          className="lnk num text-label shrink-0 whitespace-nowrap"
        >
          {link.id}
        </button>
      )}
      <span className="text-secondary text-ink-400 truncate min-w-0">
        {link.missing ? "missing" : (link.title ?? "")}
      </span>
      <span
        className={`chip ml-auto shrink-0 ${
          link.status === "done"
            ? "bg-ok/10 text-ok"
            : link.missing
              ? "bg-warn/10 text-warn"
              : "bg-ink-800 text-ink-400"
        }`}
      >
        {link.missing ? "dangling" : link.status}
      </span>
      {onRemove && (
        <button
          onClick={onRemove}
          aria-label="remove link"
          title="remove this link"
          className="shrink-0 w-5 h-5 grid place-items-center rounded text-ink-500 hover:text-fail hover:bg-fail/10"
        >
          ×
        </button>
      )}
    </li>
  );
}

export default function Drawer({
  id,
  agents,
  projects,
  pmDir,
  detail,
  onClose,
  onOpen,
  onWrite,
  onError,
}: Props) {
  const [file, setFile] = useState<string | null>(null);
  const [edit, setEdit] = useState<{
    title: string;
    status: string;
    priority: string;
    owner: string;
    component: string;
    body: string;
  } | null>(null);
  const [bodyPreview, setBodyPreview] = useState(false);
  const [linkKind, setLinkKind] = useState("blocked_by");
  const [linkTarget, setLinkTarget] = useState("");
  const [refKind, setRefKind] = useState("url");
  const [refTarget, setRefTarget] = useState("");
  const [refLabel, setRefLabel] = useState("");
  const [comment, setComment] = useState("");
  const [commentPreview, setCommentPreview] = useState(false);
  const [dropHot, setDropHot] = useState(false);
  const [history, setHistory] = useState<IssueHistoryEntry[] | null>(null);
  const [histLimit, setHistLimit] = useState(10);
  const [histErr, setHistErr] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setFile(null);
    let live = true;
    api
      .file(id)
      .then((f) => live && setFile(f))
      .catch(() => live && setFile(null));
    return () => {
      live = false;
    };
  }, [id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // A different issue starts the history list back at the first page.
  useEffect(() => {
    setHistLimit(10);
    setHistory(null);
    setHistErr(null);
  }, [id]);

  // History pages: "show more" refetches with a bigger limit.
  useEffect(() => {
    let live = true;
    api
      .history(id, histLimit)
      .then((r) => {
        if (!live) return;
        setHistory(r.history);
        setHistErr(null);
      })
      .catch((e) => {
        if (!live) return;
        setHistory([]);
        setHistErr(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [id, histLimit]);

  const sessions = (agents?.agents ?? []).filter((a) => a.on.includes(id));
  const rev = detail?.rev;
  const project = projects.find((p) => p.key === detail?.project);
  const linkRows: [string, LinkRef, (() => void)?][] = [];
  if (detail) {
    const l = detail.links;
    const rm = (kind: string, target: string) => () =>
      api
        .unlink(id, kind, target, rev)
        .then((r) => onWrite(r, `${id} unlink ${kind} ${target}`))
        .catch((e) => onError(e, "unlink"));
    if (l.parent) linkRows.push(["parent", l.parent, rm("parent", l.parent.id)]);
    for (const c of l.children) linkRows.push(["child", c, undefined]);
    for (const b of l.blocked_by)
      linkRows.push(["blocked by", b, rm("blocked_by", b.id)]);
    for (const b of l.blocks) linkRows.push(["blocks", b, undefined]);
    for (const r of l.relates) linkRows.push(["relates", r, rm("relates", r.id)]);
    if (l.duplicate_of)
      linkRows.push(["dup of", l.duplicate_of, rm("duplicate_of", l.duplicate_of.id)]);
    for (const d of l.duplicates) linkRows.push(["dup", d, undefined]);
  }
  const lastNote = detail?.notes_chain[detail.notes_chain.length - 1];
  const statusLocked = detail ? noDragReason(detail) : null;

  const startEdit = () => {
    if (!detail) return;
    setEdit({
      title: detail.title,
      status: detail.status,
      priority: detail.priority,
      owner: detail.owner ?? "",
      component: detail.component ?? "",
      body: detail.body,
    });
    setBodyPreview(false);
  };

  const saveEdit = () => {
    if (!detail || !edit) return;
    const patch: Parameters<typeof api.patch>[1] = {};
    if (edit.title.trim() && edit.title !== detail.title)
      patch.title = edit.title.trim();
    if (edit.status !== detail.status) patch.status = edit.status;
    if (edit.priority !== detail.priority) patch.priority = edit.priority;
    if (edit.owner !== (detail.owner ?? "")) patch.owner = edit.owner;
    if (edit.component !== (detail.component ?? ""))
      patch.component = edit.component;
    if (edit.body !== detail.body) patch.body = edit.body;
    if (Object.keys(patch).length === 0) {
      setEdit(null);
      return;
    }
    api
      .patch(id, patch, rev)
      .then((r) => {
        onWrite(r, `${id} saved`);
        setEdit(null);
      })
      .catch((e) => onError(e, "save"));
  };

  const sendComment = () => {
    const body = comment.trim();
    if (!body) return;
    api
      .comment(id, body, rev)
      .then((r) => {
        onWrite(r, `${id} comment`);
        setComment("");
        setCommentPreview(false);
      })
      .catch((e) => onError(e, "comment"));
  };

  const attachFiles = (files: FileList | null) => {
    if (!files) return;
    for (const f of Array.from(files)) {
      api
        .attach(id, f.name, f)
        .then((r) => onWrite(r, `${id} attach ${f.name}`))
        .catch((e) => onError(e, `attach ${f.name}`));
    }
  };

  return (
    <>
      <div
        className="fixed inset-0 bg-ink-950/70 z-20"
        onClick={onClose}
      />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[34rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        aria-label="Issue detail"
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            <div className="num text-label text-ink-500">
              {id}
              {detail
                ? ` · ${detail.project}${
                    detail.component ? " / " + detail.component : ""
                  } · ${detail.status}`
                : ""}
              {detail?.blocked ? " · blocked" : ""}
              {detail?.container ? " · container" : ""}
            </div>
            {edit ? (
              <input
                value={edit.title}
                onChange={(e) => setEdit({ ...edit, title: e.target.value })}
                className="field w-full mt-1 text-drawer font-semibold"
                aria-label="title"
              />
            ) : (
              <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
                {detail?.title ?? "…"}
              </h2>
            )}
          </div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              stroke="currentColor"
              strokeWidth="1.5"
              fill="none"
              style={{ pointerEvents: "none" }}
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-5 space-y-6">
          {detail && (
            <>
              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Fields
                  </h3>
                  <span className="kicker">writes commit to git</span>
                  {!edit && (
                    <button
                      onClick={startEdit}
                      className="lnk num text-label ml-auto"
                    >
                      edit
                    </button>
                  )}
                </div>
                {edit ? (
                  <div className="card p-3 grid grid-cols-2 gap-2.5">
                    <label className="block">
                      <span className="slabel">status</span>
                      <select
                        value={edit.status}
                        onChange={(e) =>
                          setEdit({ ...edit, status: e.target.value })
                        }
                        disabled={!!statusLocked}
                        title={statusLocked ?? undefined}
                        className="field w-full mt-1 disabled:opacity-50"
                      >
                        {STATUSES.map((s) => (
                          <option key={s}>{s}</option>
                        ))}
                      </select>
                    </label>
                    <label className="block">
                      <span className="slabel">priority</span>
                      <select
                        value={edit.priority}
                        onChange={(e) =>
                          setEdit({ ...edit, priority: e.target.value })
                        }
                        className="field w-full mt-1"
                      >
                        {PRIORITIES.map((p) => (
                          <option key={p}>{p}</option>
                        ))}
                      </select>
                    </label>
                    <label className="block">
                      <span className="slabel">owner</span>
                      <input
                        value={edit.owner}
                        onChange={(e) =>
                          setEdit({ ...edit, owner: e.target.value })
                        }
                        className="field w-full mt-1"
                        placeholder="empty clears"
                      />
                    </label>
                    <label className="block">
                      <span className="slabel">component</span>
                      <input
                        value={edit.component}
                        onChange={(e) =>
                          setEdit({ ...edit, component: e.target.value })
                        }
                        className="field w-full mt-1"
                        list={`components-${id}`}
                        placeholder="empty clears"
                      />
                      {project && (
                        <datalist id={`components-${id}`}>
                          {project.components.map((c) => (
                            <option key={c} value={c} />
                          ))}
                        </datalist>
                      )}
                    </label>
                    <div className="col-span-2">
                      <div className="flex items-baseline gap-2">
                        <span className="slabel">body (markdown)</span>
                        <button
                          onClick={() => setBodyPreview(!bodyPreview)}
                          className="lnk num text-micro"
                        >
                          {bodyPreview ? "write" : "preview"}
                        </button>
                      </div>
                      {bodyPreview ? (
                        <div className="card mt-1 p-3 text-secondary text-ink-300 min-h-[6rem]">
                          <Md text={edit.body} onOpen={onOpen} />
                        </div>
                      ) : (
                        <textarea
                          value={edit.body}
                          onChange={(e) =>
                            setEdit({ ...edit, body: e.target.value })
                          }
                          rows={7}
                          className="field w-full mt-1 !h-auto py-2 num text-label leading-relaxed"
                        />
                      )}
                    </div>
                  </div>
                ) : (
                  <div className="flex flex-wrap gap-1.5">
                    <span className="chip bg-ink-800 text-ink-300">
                      status {detail.status}
                      {detail.status_source !== "file" && (
                        <span className="text-info">
                          ·{detail.status_source}
                        </span>
                      )}
                    </span>
                    <span className="chip bg-ink-800 text-ink-300">
                      {detail.priority}
                    </span>
                    <span className="chip bg-ink-800 text-ink-300">
                      {detail.owner ? `o:${detail.owner}` : "unowned"}
                    </span>
                    {detail.component && (
                      <span className="chip bg-ink-800 text-ink-400">
                        {detail.component}
                      </span>
                    )}
                  </div>
                )}
                {statusLocked && (
                  <p className="text-micro text-ink-500 mt-1.5">
                    {statusLocked}
                  </p>
                )}
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Issue file
                  </h3>
                  <span className="kicker">the source of truth</span>
                </div>
                <div className="num text-micro text-ink-400 mb-2 break-all">
                  {relPath(detail.path, pmDir)}
                </div>
                <pre className="num text-label leading-relaxed rounded-lg border border-ink-700 bg-ink-900 p-4 text-ink-300 !whitespace-pre overflow-x-auto">
                  {file ?? detail.body}
                </pre>
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Links
                  </h3>
                  <span className="kicker num">
                    {linkRows.length} · inverses are computed
                  </span>
                </div>
                {linkRows.length > 0 && (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850 mb-2">
                    {linkRows.map(([label, link, rm], i) => (
                      <LinkRow
                        key={`${label}-${link.id}-${i}`}
                        label={label}
                        link={link}
                        onOpen={onOpen}
                        onRemove={rm}
                      />
                    ))}
                  </ul>
                )}
                <div className="flex gap-1.5">
                  <select
                    value={linkKind}
                    onChange={(e) => setLinkKind(e.target.value)}
                    className="field !h-8 text-label"
                    aria-label="link type"
                  >
                    {LINK_KINDS.map((k) => (
                      <option key={k}>{k}</option>
                    ))}
                  </select>
                  <input
                    value={linkTarget}
                    onChange={(e) => setLinkTarget(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && linkTarget.trim()) {
                        api
                          .link(id, linkKind, linkTarget.trim(), rev)
                          .then((r) => {
                            onWrite(r, `${id} ${linkKind} ${linkTarget.trim()}`);
                            setLinkTarget("");
                          })
                          .catch((e2) => onError(e2, "link"));
                      }
                    }}
                    className="field !h-8 flex-1 num text-label"
                    placeholder="CAD-16"
                  />
                  <button
                    onClick={() => {
                      const t = linkTarget.trim();
                      if (!t) return;
                      api
                        .link(id, linkKind, t, rev)
                        .then((r) => {
                          onWrite(r, `${id} ${linkKind} ${t}`);
                          setLinkTarget("");
                        })
                        .catch((e) => onError(e, "link"));
                    }}
                    disabled={!linkTarget.trim()}
                    className="h-8 px-2.5 rounded border border-ink-600 text-label text-ink-300 hover:border-accent/60 hover:text-accent disabled:opacity-40"
                  >
                    link
                  </button>
                </div>
              </section>

              <section
                onDragOver={(e) => {
                  e.preventDefault();
                  setDropHot(true);
                }}
                onDragLeave={() => setDropHot(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDropHot(false);
                  attachFiles(e.dataTransfer.files);
                }}
              >
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Artifacts
                  </h3>
                  <span className="kicker num">
                    {detail.refs.length} refs · {detail.artifacts.length} files
                  </span>
                  <button
                    onClick={() => fileInput.current?.click()}
                    className="lnk num text-label ml-auto"
                  >
                    + attach
                  </button>
                  <input
                    ref={fileInput}
                    type="file"
                    multiple
                    className="hidden"
                    onChange={(e) => {
                      attachFiles(e.target.files);
                      e.target.value = "";
                    }}
                  />
                </div>
                {dropHot && (
                  <div className="mb-2 rounded border border-dashed border-accent/60 px-3 py-2 text-label text-accent">
                    drop files to attach
                  </div>
                )}
                {detail.refs.length > 0 && (
                  <div className="flex flex-wrap gap-1.5 mb-2">
                    {detail.refs.map((r, i) => (
                      <span
                        key={i}
                        className="chip bg-ink-800 text-ink-300 !py-[.2rem]"
                      >
                        <span className="text-ink-500">{r.kind}</span>
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
                      </span>
                    ))}
                  </div>
                )}
                {detail.artifacts.length > 0 && (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.artifacts.map((f) => (
                      <li
                        key={f.name}
                        className="flex items-center gap-3 px-3 py-2"
                      >
                        <a
                          href={api.artifactUrl(id, f.name)}
                          target="_blank"
                          rel="noreferrer"
                          className="lnk num text-label"
                        >
                          artifacts/{f.name}
                        </a>
                        {IMG_EXT.test(f.name) && (
                          <img
                            src={api.artifactUrl(id, f.name)}
                            alt={f.name}
                            className="h-8 w-8 object-cover rounded border border-ink-700"
                            loading="lazy"
                          />
                        )}
                        <span className="num text-micro text-ink-500 ml-auto">
                          {fmtBytes(f.size)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                <div className="flex gap-1.5 mt-2">
                  <select
                    value={refKind}
                    onChange={(e) => setRefKind(e.target.value)}
                    className="field !h-8 text-label"
                    aria-label="ref kind"
                  >
                    {REF_KINDS.map((k) => (
                      <option key={k}>{k}</option>
                    ))}
                  </select>
                  <input
                    value={refTarget}
                    onChange={(e) => setRefTarget(e.target.value)}
                    className="field !h-8 flex-1 num text-label"
                    placeholder="https://… or path"
                  />
                  <input
                    value={refLabel}
                    onChange={(e) => setRefLabel(e.target.value)}
                    className="field !h-8 w-20 text-label"
                    placeholder="label"
                  />
                  <button
                    onClick={() => {
                      const t = refTarget.trim();
                      if (!t) return;
                      const target = /^https?:\/\//.test(t)
                        ? { url: t }
                        : { path: t };
                      api
                        .addRef(
                          id,
                          refKind,
                          target,
                          refLabel.trim() || undefined,
                          rev,
                        )
                        .then((r) => {
                          onWrite(r, `${id} ref ${refKind}`);
                          setRefTarget("");
                          setRefLabel("");
                        })
                        .catch((e) => onError(e, "ref"));
                    }}
                    disabled={!refTarget.trim()}
                    className="h-8 px-2.5 rounded border border-ink-600 text-label text-ink-300 hover:border-accent/60 hover:text-accent disabled:opacity-40"
                  >
                    ref
                  </button>
                </div>
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Activity
                  </h3>
                  <span className="kicker">
                    notes, comments &amp; file history, merged
                  </span>
                </div>
                {detail.activity.length ? (
                  <ol className="relative ml-1 border-l border-ink-700 space-y-3.5">
                    {detail.activity.map((e, i) => (
                      <li key={i} className="pl-4 relative">
                        <i className="absolute -left-[3.5px] top-[7px] w-1.5 h-1.5 rounded-full bg-ink-500" />
                        <div className="flex items-center gap-2 flex-wrap">
                          {e.kind === "note" && (
                            <>
                              <span
                                className={`chip ${
                                  NOTE_CHIP[e.note_kind ?? ""] ??
                                  "bg-ink-800 text-ink-300"
                                }`}
                              >
                                {e.note_kind ?? "note"}
                              </span>
                              <span className="num text-micro text-ink-500">
                                {fmtTime(e.at)} · agent-notes
                              </span>
                            </>
                          )}
                          {e.kind === "comment" && (
                            <>
                              <span className="chip bg-ink-800 text-ink-200">
                                {e.author}
                              </span>
                              <span className="num text-micro text-ink-500">
                                {fmtTime(e.at)} · comment
                              </span>
                            </>
                          )}
                          {e.kind === "commit" && (
                            <>
                              <span className="chip bg-ink-800 text-ink-300">
                                {e.commit}
                              </span>
                              <span className="num text-micro text-ink-500">
                                {fmtTime(e.at)} · git
                              </span>
                            </>
                          )}
                        </div>
                        <div className="text-secondary text-ink-300 mt-1 leading-[1.5]">
                          {e.kind === "note" && (e.title ?? e.name)}
                          {e.kind === "comment" && (
                            <Md text={e.body ?? ""} onOpen={onOpen} />
                          )}
                          {e.kind === "commit" && e.subject}
                        </div>
                      </li>
                    ))}
                  </ol>
                ) : (
                  <p className="text-secondary text-ink-500">Nothing yet.</p>
                )}
                {lastNote && detail.status_source === "notes" && (
                  <p className="text-label text-ink-500 mt-3 leading-[1.5]">
                    Latest tagged note is a {lastNote.kind}, so the board shows{" "}
                    <span className="text-ink-200">{detail.status}</span>. Once
                    M3 ships, status comes from the job state instead.
                  </p>
                )}
                {detail.container && (
                  <p className="text-label text-ink-500 mt-3 leading-[1.5]">
                    A container is never dispatched. Its status rolls up from
                    its children.
                  </p>
                )}

                <div className="mt-3">
                  <div className="flex items-baseline gap-2 mb-1">
                    <span className="slabel">comment as operator</span>
                    <button
                      onClick={() => setCommentPreview(!commentPreview)}
                      className="lnk num text-micro"
                    >
                      {commentPreview ? "write" : "preview"}
                    </button>
                    <span className="num text-micro text-ink-600 ml-auto">
                      ⌘/Ctrl+Enter sends
                    </span>
                  </div>
                  {commentPreview ? (
                    <div className="card p-3 text-secondary text-ink-300 min-h-[4rem]">
                      {comment.trim() ? (
                        <Md text={comment} onOpen={onOpen} />
                      ) : (
                        <span className="text-ink-500">nothing to preview</span>
                      )}
                    </div>
                  ) : (
                    <textarea
                      value={comment}
                      onChange={(e) => setComment(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                          e.preventDefault();
                          sendComment();
                        }
                      }}
                      rows={3}
                      className="field w-full !h-auto py-2 text-secondary"
                      placeholder="markdown — raw html stays inert"
                    />
                  )}
                </div>
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Sessions
                  </h3>
                  <span className="kicker">derived from the daemon</span>
                </div>
                {(detail.agents?.length ?? 0) > 0 ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.agents!.map((a) => (
                      <li
                        key={`${a.alias}:${a.task}`}
                        className="flex items-center gap-2.5 px-3 py-2.5 flex-wrap"
                      >
                        <i
                          className={`w-1.5 h-1.5 rounded-full ${
                            a.state === "attention"
                              ? "bg-fail"
                              : a.message
                                ? "bg-info"
                                : "bg-ink-600"
                          }`}
                        />
                        <span className="num text-label text-ink-200">
                          {a.alias}
                        </span>
                        <span className="num text-micro text-ink-500">
                          {a.task} · {a.task_state}
                          {a.message ? ` · ${a.message}` : ""}
                        </span>
                        {a.resume && (
                          <code className="num text-micro text-ink-400 ml-auto">
                            {a.resume}
                          </code>
                        )}
                      </li>
                    ))}
                  </ul>
                ) : sessions.length ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {sessions.map((a) => (
                      <li
                        key={a.alias}
                        className="flex items-center gap-2.5 px-3 py-2.5"
                      >
                        <i className="w-1.5 h-1.5 rounded-full bg-info" />
                        <span className="num text-label text-ink-200">
                          {a.alias}
                        </span>
                        <span className="num text-micro text-ink-500">
                          {a.provider} · {a.endpoint_kind} · running
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-secondary text-ink-500">
                    No live session. The issue file never stores sessions.
                  </p>
                )}
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    History
                  </h3>
                  <span className="kicker">parsed from git log</span>
                  {history !== null && history.length >= histLimit && (
                    <button
                      onClick={() => setHistLimit(histLimit + 20)}
                      className="lnk num text-label ml-auto"
                    >
                      show more
                    </button>
                  )}
                </div>
                {histErr ? (
                  <p className="text-secondary text-ink-500">
                    History unavailable — {histErr}
                  </p>
                ) : history === null ? (
                  <p className="text-secondary text-ink-500">Loading…</p>
                ) : history.length === 0 ? (
                  <p className="text-secondary text-ink-500">
                    No git history yet.
                  </p>
                ) : (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {history.map((h) => (
                      <li
                        key={h.sha}
                        className="flex items-center gap-2.5 px-3 py-2"
                      >
                        <span className="num text-micro text-ink-500 shrink-0 whitespace-nowrap">
                          {fmtTime(h.at)}
                        </span>
                        <span className="chip bg-ink-800 text-ink-300 shrink-0 max-w-[9rem] truncate">
                          {h.by}
                        </span>
                        <span
                          className={`text-secondary truncate min-w-0 ${
                            h.kind === "other" ? "text-ink-500" : "text-ink-300"
                          }`}
                        >
                          {h.summary}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            </>
          )}
        </div>

        <footer className="shrink-0 border-t border-ink-700 px-5 py-3 flex items-center gap-3 bg-ink-875">
          <span className="text-label text-ink-500 leading-[1.4]">
            {edit
              ? "Editing fields — Save commits once as operator (ui)."
              : "Edits, comments and attaches write through the board API. Dispatch waits for a token."}
          </span>
          <div className="ml-auto flex gap-2 shrink-0">
            {edit ? (
              <>
                <button
                  onClick={() => setEdit(null)}
                  className="h-9 px-3 rounded border border-ink-600 text-secondary text-ink-300 hover:border-ink-500"
                >
                  Cancel
                </button>
                <button
                  onClick={saveEdit}
                  className="h-9 px-3 rounded bg-accent text-ink-950 text-secondary font-medium"
                >
                  Save
                </button>
              </>
            ) : (
              <>
                <button
                  onClick={sendComment}
                  disabled={!comment.trim()}
                  className="h-9 px-3 rounded border border-ink-600 text-secondary text-ink-300 hover:border-accent/60 hover:text-accent disabled:opacity-45 disabled:cursor-not-allowed"
                >
                  Comment
                </button>
                <button
                  disabled
                  title="dispatch waits for a token — not in I2"
                  className="h-9 px-3 rounded bg-accent text-ink-950 text-secondary font-medium opacity-40 cursor-not-allowed"
                >
                  Kick off
                </button>
              </>
            )}
          </div>
        </footer>
      </aside>
    </>
  );
}
