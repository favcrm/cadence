import { useCallback, useEffect, useState } from "react";
import { api, ApiError } from "../api";
import type { MemoryCard, MemoryDetail } from "../types";
import Md from "./Md";

const STATUSES = ["", "proposed", "accepted", "rejected", "superseded"];
const TYPES = ["", "rule", "gotcha", "decision", "recipe"];

function statusTone(status: string): string {
  switch (status) {
    case "accepted":
      return "bg-accent/10 text-accent";
    case "proposed":
      return "bg-warn/10 text-warn";
    case "rejected":
      return "bg-fail/10 text-fail";
    default:
      return "bg-ink-800 text-ink-400";
  }
}

function scopeLine(m: MemoryCard): string {
  const bits: string[] = [];
  if (m.scope.project) bits.push("project-wide");
  if (m.scope.components.length) bits.push(`comp: ${m.scope.components.join(", ")}`);
  if (m.scope.paths.length) bits.push(`paths: ${m.scope.paths.join(", ")}`);
  if (m.scope.providers.length) bits.push(`prov: ${m.scope.providers.join(", ")}`);
  if (m.scope.tags.length) bits.push(`tags: ${m.scope.tags.join(", ")}`);
  return bits.join(" · ") || "no scope";
}

export default function Memory({
  project,
  onError,
}: {
  project: string;
  onError: (e: unknown, verb: string) => void;
}) {
  const [mems, setMems] = useState<MemoryCard[]>([]);
  const [status, setStatus] = useState("");
  const [kind, setKind] = useState("");
  const [open, setOpen] = useState<string | null>(null); // project/slug
  const [detail, setDetail] = useState<MemoryDetail | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    api
      .memories({
        project: project === "all" ? undefined : project,
        status: status || undefined,
        type: kind || undefined,
      })
      .then((r) => setMems(r.memories))
      .catch((e) => onError(e, "memory list"));
  }, [project, status, kind, onError]);

  useEffect(refresh, [refresh]);

  const openDetail = useCallback((m: MemoryCard) => {
    const key = `${m.project}/${m.slug}`;
    setOpen((cur) => (cur === key ? null : key));
    setDraft(null);
    api
      .memory(m.project, m.slug)
      .then((d) => {
        setDetail(d);
        setDraft(d.body);
      })
      .catch(() => setDetail(null));
  }, []);

  const review = useCallback(
    (m: MemoryDetail, verb: "accept" | "reject") => {
      setBusy(true);
      const edited = verb === "accept" && draft !== null && draft !== m.body;
      const call =
        verb === "accept"
          ? (p: string, s: string) => api.memoryAccept(p, s, edited ? draft! : undefined)
          : api.memoryReject;
      call(m.project, m.slug)
        .then((r) => {
          setDetail(r.memory);
          refresh();
        })
        .catch((e) => onError(e as ApiError, `memory ${verb}`))
        .finally(() => setBusy(false));
    },
    [onError, refresh, draft],
  );

  return (
    <div className="px-4 lg:px-8 py-4 max-w-[1100px]">
      <div className="flex items-center gap-2 mb-3">
        <span className="slabel">status</span>
        {STATUSES.map((s) => (
          <button
            key={s || "all"}
            onClick={() => setStatus(s)}
            className={`chip ${
              status === s
                ? "bg-accent/10 text-accent"
                : "bg-ink-800 text-ink-400 hover:text-ink-200"
            }`}
          >
            {s || "all"}
          </button>
        ))}
        <span className="slabel ml-3">type</span>
        {TYPES.map((t) => (
          <button
            key={t || "all"}
            onClick={() => setKind(t)}
            className={`chip ${
              kind === t
                ? "bg-accent/10 text-accent"
                : "bg-ink-800 text-ink-400 hover:text-ink-200"
            }`}
          >
            {t || "all"}
          </button>
        ))}
      </div>

      {mems.length === 0 && (
        <div className="card px-4 py-6 text-center text-ink-500 text-sm">
          no memories — `cadence memory propose` records the first one
        </div>
      )}

      <div className="grid gap-2">
        {mems.map((m) => {
          const key = `${m.project}/${m.slug}`;
          const isOpen = open === key;
          return (
            <div key={key} className="card px-4 py-3">
              <button
                className="w-full text-left grid gap-1"
                onClick={() => openDetail(m)}
              >
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="num text-[13px] text-ink-100">{m.slug}</span>
                  <span className={`chip ${statusTone(m.status)}`}>{m.status}</span>
                  <span className="chip bg-ink-800 text-ink-300">{m.type}</span>
                  <span className="chip bg-ink-800 text-ink-500">{m.confidence}</span>
                  <span className="num text-micro text-ink-500 ml-auto">
                    {m.project}
                    {m.verified_at ? ` · verified ${m.verified_at.slice(0, 10)}` : ""}
                  </span>
                </div>
                <div className="text-[13px] text-ink-300">{m.fact}</div>
                <div className="num text-micro text-ink-500 truncate">
                  {scopeLine(m)}
                  {m.supersedes ? ` · supersedes ${m.supersedes}` : ""}
                </div>
              </button>

              {isOpen && detail && detail.slug === m.slug && (
                <div className="mt-3 pt-3 border-t border-ink-700">
                  <div className="text-[13px] text-ink-200 [&_p]:mb-2">
                    <Md text={detail.body} />
                  </div>
                  <div className="num text-micro text-ink-500 mt-2">
                    {detail.author ? `by ${detail.author} · ` : ""}
                    {detail.source ? `source ${detail.source} · ` : ""}
                    {detail.path}
                  </div>
                  {detail.status === "proposed" && (
                    <div className="mt-3">
                      <textarea
                        value={draft ?? detail.body}
                        onChange={(e) => setDraft(e.target.value)}
                        rows={8}
                        className="w-full bg-ink-900 border border-ink-700 rounded px-2 py-1.5 text-[13px] text-ink-200 font-mono focus:outline-none focus:border-accent/50"
                      />
                    </div>
                  )}
                  {detail.status === "proposed" && (
                    <div className="flex gap-2 mt-3">
                      <button
                        disabled={busy}
                        onClick={() => review(detail, "accept")}
                        className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
                      >
                        accept
                      </button>
                      <button
                        disabled={busy}
                        onClick={() => review(detail, "reject")}
                        className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors"
                      >
                        reject
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
