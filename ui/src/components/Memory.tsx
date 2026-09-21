import { useCallback, useEffect, useRef, useState } from "react";
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

function errText(e: unknown): string {
  return e instanceof ApiError ? e.message : String(e);
}

type DetailState = {
  key: string;
  data?: MemoryDetail;
  error?: string;
};

export default function Memory({
  project,
  onError,
}: {
  project: string;
  onError: (e: unknown, verb: string) => void;
}) {
  // null = loading; [] = resolved empty (or failed — see listErr).
  const [mems, setMems] = useState<MemoryCard[] | null>(null);
  const [loadErrs, setLoadErrs] = useState<string[]>([]);
  const [listErr, setListErr] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const [status, setStatus] = useState("");
  const [kind, setKind] = useState("");
  const [open, setOpen] = useState<string | null>(null); // project/slug
  const [detail, setDetail] = useState<DetailState | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The detail key the last openDetail asked for — a late response for
  // a previous card (or one already closed) is dropped here, never
  // rendered under the wrong row.
  const wanted = useRef<string | null>(null);

  const refresh = useCallback(() => {
    let cancelled = false;
    setMems(null);
    setListErr(null);
    setLoadErrs([]);
    api
      .memories({
        project: project === "all" ? undefined : project,
        status: status || undefined,
        type: kind || undefined,
      })
      .then((r) => {
        if (cancelled) return;
        setMems(r.memories);
        setLoadErrs(r.memory_errors ?? []);
      })
      .catch((e) => {
        if (cancelled) return;
        setMems([]);
        setListErr(errText(e));
        onError(e, "memory list");
      });
    return () => {
      cancelled = true;
    };
  }, [project, status, kind, onError, tick]);

  useEffect(refresh, [refresh]);

  const loadDetail = useCallback((m: MemoryCard) => {
    const key = `${m.project}/${m.slug}`;
    wanted.current = key;
    setDetail({ key });
    api
      .memory(m.project, m.slug)
      .then((d) => {
        if (wanted.current === key) setDetail({ key, data: d });
      })
      .catch((e) => {
        if (wanted.current === key) setDetail({ key, error: errText(e) });
      });
  }, []);

  const openDetail = useCallback(
    (m: MemoryCard) => {
      const key = `${m.project}/${m.slug}`;
      if (open === key) {
        wanted.current = null;
        setOpen(null);
        setDetail(null);
        setDraft(null);
        return;
      }
      setOpen(key);
      setDraft(null);
      loadDetail(m);
    },
    [open, loadDetail],
  );

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
          const key = `${m.project}/${m.slug}`;
          if (wanted.current === key) setDetail({ key, data: r.memory });
          setTick((t) => t + 1);
        })
        .catch((e) => onError(e as ApiError, `memory ${verb}`))
        .finally(() => setBusy(false));
    },
    [onError, draft],
  );

  const filtered = status !== "" || kind !== "";

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

      {listErr !== null ? (
        <div className="card px-4 py-6 text-center text-sm">
          <p className="text-fail">memory list unavailable — {listErr}</p>
          <button
            className="chip mt-3 bg-ink-800 text-ink-300 hover:text-ink-100"
            onClick={() => setTick((t) => t + 1)}
          >
            retry
          </button>
        </div>
      ) : mems === null ? (
        <div className="card px-4 py-6 text-center text-ink-500 text-sm">
          Loading…
        </div>
      ) : mems.length === 0 ? (
        <div className="card px-4 py-6 text-center text-ink-500 text-sm">
          {filtered
            ? "no memories match the current filters"
            : "no memories yet — agents propose lessons with `cadence memory propose`; a curator accepts them before they reach a dispatch"}
        </div>
      ) : null}

      {loadErrs.length > 0 && (
        <div className="card px-4 py-2.5 mb-2 border-warn/30 text-warn text-[13px]">
          {loadErrs.length} memory file{loadErrs.length === 1 ? "" : "s"} failed
          to load — {loadErrs[0]}
          {loadErrs.length > 1 ? ` (and ${loadErrs.length - 1} more)` : ""}
        </div>
      )}

      <div className="grid gap-2">
        {(mems ?? []).map((m) => {
          const key = `${m.project}/${m.slug}`;
          const isOpen = open === key;
          const d = isOpen && detail?.key === key ? detail : null;
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

              {isOpen && (
                <div className="mt-3 pt-3 border-t border-ink-700">
                  {d?.data ? (
                    <>
                      <div className="text-[13px] text-ink-200 [&_p]:mb-2">
                        <Md text={d.data.body} />
                      </div>
                      <div className="num text-micro text-ink-500 mt-2">
                        {d.data.author ? `by ${d.data.author} · ` : ""}
                        {d.data.source ? `source ${d.data.source} · ` : ""}
                        {d.data.path}
                      </div>
                      {d.data.status === "proposed" && (
                        <div className="mt-3">
                          <textarea
                            value={draft ?? d.data.body}
                            onChange={(e) => setDraft(e.target.value)}
                            rows={8}
                            className="w-full bg-ink-900 border border-ink-700 rounded px-2 py-1.5 text-[13px] text-ink-200 font-mono focus:outline-none focus:border-accent/50"
                          />
                        </div>
                      )}
                      {d.data.status === "proposed" && (
                        <div className="flex gap-2 mt-3">
                          <button
                            disabled={busy}
                            onClick={() => review(d.data!, "accept")}
                            className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
                          >
                            accept
                          </button>
                          <button
                            disabled={busy}
                            onClick={() => review(d.data!, "reject")}
                            className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors"
                          >
                            reject
                          </button>
                        </div>
                      )}
                    </>
                  ) : d?.error ? (
                    <div className="text-[13px]">
                      <p className="text-fail">memory unavailable — {d.error}</p>
                      <button
                        className="chip mt-2 bg-ink-800 text-ink-300 hover:text-ink-100"
                        onClick={() => loadDetail(m)}
                      >
                        retry
                      </button>
                    </div>
                  ) : (
                    <p className="text-secondary text-ink-500">Loading…</p>
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
