import { useEffect, useRef, useState } from "react";
import { api, type WriteResp } from "../../lib/api";
import { fmtBytes, fmtTime } from "../../lib/fmt";
import { notesStatusSentence } from "../../lib/uxCopy";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import Select from "../../ui/Select";
import { IconClose } from "../../ui/icons";
import { noDragReason } from "./Card";
import type {
  AgentsPayload,
  IssueDetail,
  IssueHistoryEntry,
  LinkRef,
  Project,
} from "../../lib/types";

const NOTE_CHIP: Record<string, string> = {
  kickoff: "bg-info/10 text-info",
  qa: "bg-warn/10 text-warn",
  verdict: "bg-ok/10 text-ok",
};

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];
const LINK_KINDS = ["blocked_by", "relates", "parent", "duplicate_of"];
/// `ui, api  infra` → `["ui", "api", "infra"]`.
const splitTags = (text: string) => text.split(/[\s,]+/).filter(Boolean);

const REF_KINDS = ["pr", "commit", "note", "preview", "message", "url"];
const IMG_EXT = /\.(png|jpe?g|gif|webp)$/i;

interface Props {
  id: string;
  agents: AgentsPayload | null;
  projects: Project[];
  pmDir?: string;
  detail: IssueDetail | null;
  readOnly: boolean;
  /** Operator session only. A signed-in member keeps other writes and cannot dispatch. */
  canKickoff: boolean;
  actor: string;
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
  readOnly,
  canKickoff,
  actor,
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
    /** Comma- or space-separated while editing. */
    tags: string;
    body: string;
  } | null>(null);
  const [sourceView, setSourceView] = useState(false);
  const [bodyPreview, setBodyPreview] = useState(false);
  const [linkKind, setLinkKind] = useState("blocked_by");
  const [linkTarget, setLinkTarget] = useState("");
  const [refKind, setRefKind] = useState("url");
  const [refTarget, setRefTarget] = useState("");
  const [refLabel, setRefLabel] = useState("");
  const [comment, setComment] = useState("");
  const [kickConfirm, setKickConfirm] = useState(false);
  const [kicking, setKicking] = useState(false);
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
    const rm = (kind: string, target: string) =>
      readOnly
        ? undefined
        : () =>
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
  const notesStatus =
    detail && lastNote
      ? notesStatusSentence(detail.status_source, lastNote.kind, detail.status)
      : null;
  const statusLocked = detail ? noDragReason(detail) : null;

  const startEdit = () => {
    if (!detail) return;
    setEdit({
      title: detail.title,
      status: detail.status,
      priority: detail.priority,
      owner: detail.owner ?? "",
      component: detail.component ?? "",
      tags: (detail.tags ?? []).join(", "),
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
    // The server sorts, de-duplicates and validates; compare as sets
    // so retyping the same tags in another order is not a write.
    const tags = [...new Set(splitTags(edit.tags))].sort();
    if (tags.join(",") !== [...(detail.tags ?? [])].sort().join(","))
      patch.tags = tags;
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

  const kickOff = () => {
    if (!canKickoff) return;
    if (!kickConfirm) {
      setKickConfirm(true);
      return;
    }
    setKicking(true);
    api
      .kickoffOptions(id)
      .then((opts) => {
        const group = opts.defaults.group ?? "";
        const provider = opts.defaults.provider ?? "";
        if (!group || !provider) {
          throw new Error(
            "Kick off needs a group and a provider — set a worker default, or use the issue page.",
          );
        }
        const model = opts.defaults.model ?? undefined;
        const effort = opts.defaults.effort ?? undefined;
        return api.kickoff(id, { group, provider, model, effort });
      })
      .then((r) => {
        onWrite(r, `${id} kick off`);
        setKickConfirm(false);
      })
      .catch((e) => onError(e, "kick off"))
      .finally(() => setKicking(false));
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
        className="fixed inset-0 bg-scrim z-20"
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
            <IconClose style={{ pointerEvents: "none" }} />
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
                  {!edit && !readOnly && (
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
                      <Select
                        full
                        className="mt-1"
                        value={edit.status}
                        onChange={(status) => setEdit({ ...edit, status })}
                        disabled={!!statusLocked}
                        title={statusLocked ?? undefined}
                        aria-label="status"
                        options={STATUSES.map((s) => ({ value: s, label: s }))}
                      />
                    </label>
                    <label className="block">
                      <span className="slabel">priority</span>
                      <Select
                        full
                        className="mt-1"
                        value={edit.priority}
                        onChange={(priority) => setEdit({ ...edit, priority })}
                        aria-label="priority"
                        options={PRIORITIES.map((p) => ({ value: p, label: p }))}
                      />
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
                    <label className="block col-span-2">
                      <span className="slabel">tags</span>
                      <input
                        value={edit.tags}
                        onChange={(e) =>
                          setEdit({ ...edit, tags: e.target.value })
                        }
                        className="field w-full mt-1"
                        placeholder="comma separated — empty clears"
                      />
                      {(project?.tags?.length ?? 0) > 0 && (
                        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                          <span className="kicker">declared</span>
                          {project!.tags!.map((tag) => {
                            const cur = splitTags(edit.tags);
                            const on = cur.includes(tag);
                            return (
                              <button
                                key={tag}
                                type="button"
                                aria-pressed={on}
                                onClick={() =>
                                  setEdit({
                                    ...edit,
                                    tags: (on
                                      ? cur.filter((t) => t !== tag)
                                      : [...cur, tag]
                                    ).join(", "),
                                  })
                                }
                                className={`chip !py-[.1rem] ${
                                  on
                                    ? "bg-accent/10 text-accent"
                                    : "bg-ink-800 text-ink-400 hover:text-ink-200"
                                }`}
                              >
                                {tag}
                              </button>
                            );
                          })}
                        </div>
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
                    {(detail.tags ?? []).map((tag) => (
                      <span key={tag} className="chip bg-ink-800 text-ink-400">
                        #{tag}
                      </span>
                    ))}
                  </div>
                )}
                {statusLocked && (
                  <p className="text-micro text-ink-500 mt-1.5">
                    {statusLocked}
                  </p>
                )}
              </section>

              <section>
                <div className="flex flex-wrap items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Issue body
                  </h3>
                  <span className="kicker">{sourceView ? "raw source" : "rendered markdown"}</span>
                  <button
                    className="lnk num text-micro ml-auto"
                    onClick={() => setSourceView((open) => !open)}
                    aria-pressed={sourceView}
                  >
                    {sourceView ? "read rendered" : "view source"}
                  </button>
                </div>
                {sourceView ? (
                  <>
                    <div className="num text-micro text-ink-400 mb-2 break-all">
                      {relPath(detail.path, pmDir)}
                    </div>
                    <pre className="num text-label leading-relaxed rounded-lg border border-ink-700 bg-ink-900 p-4 text-ink-300 !whitespace-pre-wrap break-words overflow-x-auto">
                      {file ?? detail.body}
                    </pre>
                  </>
                ) : (
                  <div className="issue-reader rounded-lg border border-ink-700 bg-ink-900 px-4 py-3 text-secondary text-ink-300">
                    <Md text={detail.body} onOpen={onOpen} />
                  </div>
                )}
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
                {!readOnly && (
                  <div className="flex gap-1.5">
                    <Select
                      value={linkKind}
                      onChange={setLinkKind}
                      aria-label="link type"
                      options={LINK_KINDS.map((k) => ({ value: k, label: k }))}
                    />
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
                    <Button
                      size="sm"
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
                    >
                      link
                    </Button>
                  </div>
                )}
              </section>

              <section
                onDragOver={(e) => {
                  if (readOnly) return;
                  e.preventDefault();
                  setDropHot(true);
                }}
                onDragLeave={() => setDropHot(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDropHot(false);
                  if (!readOnly) attachFiles(e.dataTransfer.files);
                }}
              >
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Artifacts
                  </h3>
                  <span className="kicker num">
                    {detail.refs.length} refs · {detail.artifacts.length} files
                  </span>
                  {!readOnly && (
                    <button
                      onClick={() => fileInput.current?.click()}
                      className="lnk num text-label ml-auto"
                    >
                      + attach
                    </button>
                  )}
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
                        className={`chip bg-ink-800 !py-[.2rem] ${
                          r.closed ? "text-ink-500 line-through" : "text-ink-300"
                        }`}
                        title={r.closed ? "closed — kept as history" : undefined}
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
                          <span>
                            {r.kind === "branch" && r.label
                              ? `${r.label}: ${r.path}`
                              : r.kind === "worktree" && r.path
                                ? (r.path.split(".cadence/wt/")[1]
                                    ? `wt/${r.path.split(".cadence/wt/")[1]}`
                                    : r.path)
                                : (r.label ?? r.path)}
                          </span>
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
                        className="flex items-center gap-3 px-3 py-2 min-w-0"
                      >
                        <a
                          href={api.artifactUrl(id, f.name)}
                          target="_blank"
                          rel="noreferrer"
                          className="lnk num text-label min-w-0 break-all"
                        >
                          artifacts/{f.name}
                        </a>
                        {IMG_EXT.test(f.name) && (
                          <img
                            src={api.artifactUrl(id, f.name)}
                            alt={f.name}
                            className="h-8 w-8 shrink-0 object-cover rounded border border-ink-700"
                            loading="lazy"
                          />
                        )}
                        <span className="num text-micro text-ink-500 ml-auto shrink-0">
                          {fmtBytes(f.size)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                {!readOnly && (
                  <div className="flex flex-wrap gap-1.5 mt-2">
                    <Select
                      value={refKind}
                      onChange={setRefKind}
                      aria-label="ref kind"
                      options={REF_KINDS.map((k) => ({ value: k, label: k }))}
                    />
                    <input
                      value={refTarget}
                      onChange={(e) => setRefTarget(e.target.value)}
                      className="field !h-8 flex-1 min-w-0 basis-40 num text-label"
                      placeholder="https://… or path"
                    />
                    <input
                      value={refLabel}
                      onChange={(e) => setRefLabel(e.target.value)}
                      className="field !h-8 w-20 text-label"
                      placeholder="label"
                    />
                    <Button
                      size="sm"
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
                    >
                      ref
                    </Button>
                  </div>
                )}
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
                {notesStatus && (
                  <p className="text-label text-ink-500 mt-3 leading-[1.5]">
                    {notesStatus}
                  </p>
                )}
                {detail.container && (
                  <p className="text-label text-ink-500 mt-3 leading-[1.5]">
                    A container is never dispatched. Its status rolls up from
                    its children.
                  </p>
                )}

                {!readOnly && (
                  <div className="mt-3">
                    <div className="flex items-baseline gap-2 mb-1">
                      <span className="slabel">comment as {actor}</span>
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
                )}
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
                    Commits
                  </h3>
                  <span className="kicker">from project repos</span>
                </div>
                {(detail.commits?.length ?? 0) > 0 ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.commits!.map((c) => (
                      <li
                        key={`${c.repo}:${c.sha}`}
                        className="flex items-center gap-2.5 px-3 py-2"
                      >
                        <span className="num text-micro text-ink-500 shrink-0 whitespace-nowrap">
                          {fmtTime(c.at)}
                        </span>
                        <span className="num text-micro text-accent-300 shrink-0">
                          {c.sha.slice(0, 7)}
                        </span>
                        <span className="text-secondary text-ink-300 truncate min-w-0">
                          {c.subject}
                        </span>
                        {c.on_default === false && (
                          <span
                            className="chip bg-ink-800 text-ink-400 shrink-0"
                            title="Not on the default branch"
                          >
                            branch
                          </span>
                        )}
                        <span className="text-micro text-ink-500 shrink-0 truncate max-w-[8rem]">
                          {c.repo}
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (detail.commits_skipped?.length ?? 0) > 0 ? (
                  <p className="text-secondary text-ink-500">
                    No commits listed —{" "}
                    {detail.commits_skipped!.map(
                      (s) => `${s.repo} (${s.reason})`,
                    ).join(", ")}
                    .
                  </p>
                ) : (
                  <p className="text-secondary text-ink-500">
                    No code commits yet — tag one with an `Issue: {id}`
                    trailer or `({id})` in the subject.
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
              ? `Editing fields — Save commits once as ${actor}.`
              : readOnly
                ? "Read-only — the server refuses every write."
                : "Edits and comments write through the board API. Kick off joins a worker and dispatches."}
          </span>
          <div className="ml-auto flex gap-2 shrink-0">
            {edit ? (
              <>
                <Button onClick={() => setEdit(null)}>Cancel</Button>
                <Button variant="primary" onClick={saveEdit}>
                  Save
                </Button>
              </>
            ) : (
              !readOnly && (
                <>
                  <Button onClick={sendComment} disabled={!comment.trim()}>
                    Comment
                  </Button>
                  <Button
                    variant="primary"
                    onClick={kickOff}
                    disabled={kicking || !canKickoff}
                    loading={kicking}
                    title={
                      !canKickoff
                        ? "Kick off is the operator's decision"
                        : kickConfirm
                          ? "Join a worker and dispatch this issue"
                          : "Confirm before kick off"
                    }
                  >
                    {kicking ? "Kicking off…" : kickConfirm ? "Confirm kick off" : "Kick off"}
                  </Button>
                </>
              )
            )}
          </div>
        </footer>
      </aside>
    </>
  );
}
