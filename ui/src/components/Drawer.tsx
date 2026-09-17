import { useEffect, useState } from "react";
import { api } from "../api";
import { fmtBytes, fmtTime } from "../fmt";
import Md from "./Md";
import type { AgentsPayload, IssueDetail, LinkRef } from "../types";

const NOTE_CHIP: Record<string, string> = {
  kickoff: "bg-info/10 text-info",
  qa: "bg-warn/10 text-warn",
  verdict: "bg-ok/10 text-ok",
};

interface Props {
  id: string;
  agents: AgentsPayload | null;
  pmDir?: string;
  onClose: () => void;
  onOpen: (id: string) => void;
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
}: {
  label: string;
  link: LinkRef;
  onOpen: (id: string) => void;
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
    </li>
  );
}

export default function Drawer({ id, agents, pmDir, onClose, onOpen }: Props) {
  const [detail, setDetail] = useState<IssueDetail | null>(null);
  const [file, setFile] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setFile(null);
    setError(null);
    let live = true;
    api
      .issue(id)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setError(String(e.message ?? e)));
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

  const sessions = (agents?.agents ?? []).filter((a) => a.on.includes(id));
  const linkRows: [string, LinkRef][] = [];
  if (detail) {
    const l = detail.links;
    if (l.parent) linkRows.push(["parent", l.parent]);
    for (const c of l.children) linkRows.push(["child", c]);
    for (const b of l.blocked_by) linkRows.push(["blocked by", b]);
    for (const b of l.blocks) linkRows.push(["blocks", b]);
    for (const r of l.relates) linkRows.push(["relates", r]);
    if (l.duplicate_of) linkRows.push(["dup of", l.duplicate_of]);
    for (const d of l.duplicates) linkRows.push(["dup", d]);
  }
  const lastNote = detail?.notes_chain[detail.notes_chain.length - 1];

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
          <div className="min-w-0">
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
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
              {detail?.title ?? "…"}
            </h2>
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
          {error && (
            <p className="text-secondary text-fail">{error}</p>
          )}
          {detail && (
            <>
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
                {linkRows.length ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {linkRows.map(([label, link], i) => (
                      <LinkRow
                        key={`${label}-${link.id}-${i}`}
                        label={label}
                        link={link}
                        onOpen={onOpen}
                      />
                    ))}
                  </ul>
                ) : (
                  <p className="text-secondary text-ink-500">None.</p>
                )}
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Artifacts
                  </h3>
                  <span className="kicker num">
                    {detail.refs.length} refs · {detail.artifacts.length} files
                  </span>
                </div>
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
                        <span className="num text-label text-ink-200">
                          artifacts/{f.name}
                        </span>
                        <span className="num text-micro text-ink-500 ml-auto">
                          {fmtBytes(f.size)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                {detail.refs.length === 0 && detail.artifacts.length === 0 && (
                  <p className="text-secondary text-ink-500">None.</p>
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
              </section>

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Sessions
                  </h3>
                  <span className="kicker">derived from the daemon</span>
                </div>
                {sessions.length ? (
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
            </>
          )}
        </div>

        <footer className="shrink-0 border-t border-ink-700 px-5 py-3 flex items-center gap-3 bg-ink-875">
          <span className="text-label text-ink-500 leading-[1.4]">
            Read-only in iteration 1. Comment, attach and edit arrive in I2.
            Dispatch waits for a token.
          </span>
          <div className="ml-auto flex gap-2 shrink-0">
            <button
              disabled
              className="h-9 px-3 rounded border border-ink-600 text-secondary text-ink-300 opacity-45 cursor-not-allowed"
            >
              Comment
            </button>
            <button
              disabled
              className="h-9 px-3 rounded bg-accent text-ink-950 text-secondary font-medium opacity-40 cursor-not-allowed"
            >
              Kick off
            </button>
          </div>
        </footer>
      </aside>
    </>
  );
}
