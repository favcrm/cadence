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

type QuorumView = {
  eligible: boolean | null;
  reason: string;
  mode: "acceptance" | "retrieval";
};

function quorumView(m: MemoryCard): QuorumView {
  const mode = m.status === "proposed" ? "acceptance" : "retrieval";
  if (!m.quorum) {
    return {
      eligible: null,
      reason: "verification unavailable — this server did not report quorum",
      mode,
    };
  }

  const check = mode === "acceptance" ? m.quorum.accept : m.quorum;
  if (!check || typeof check.eligible !== "boolean") {
    return {
      eligible: null,
      reason: "verification unavailable — this server did not report quorum",
      mode,
    };
  }

  return {
    eligible: check.eligible,
    reason: check.reason || "server did not provide a quorum reason",
    mode,
  };
}

function quorumTone(eligible: boolean | null): string {
  if (eligible === true) return "bg-accent/10 text-accent";
  if (eligible === false) return "bg-warn/10 text-warn";
  return "bg-ink-800 text-ink-400";
}

function quorumLabel(q: QuorumView): string {
  if (q.eligible === null) return "verification unavailable";
  if (q.mode === "acceptance") {
    return q.eligible ? "awaiting PM finalization" : "review blocked";
  }
  return q.eligible ? "available to agents" : "not available to agents";
}

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
        return;
      }
      setOpen(key);
      loadDetail(m);
    },
    [open, loadDetail],
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
            : "no memories yet — authenticated native agents propose lessons with `cadence memory propose`; two independent reviews and PM finalization are required before dispatch"}
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
          const q = quorumView(m);
          const detailQuorum = d?.data ? quorumView(d.data) : null;
          return (
            <div key={key} className="card px-4 py-3">
              <button
                className="w-full text-left grid gap-1"
                onClick={() => openDetail(m)}
              >
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="num text-[13px] text-ink-100 min-w-0 break-all">
                    {m.slug}
                  </span>
                  <span className={`chip ${statusTone(m.status)}`}>{m.status}</span>
                  <span className="chip bg-ink-800 text-ink-300">{m.type}</span>
                  <span className="chip bg-ink-800 text-ink-500">{m.confidence}</span>
                  <span className={`chip ${quorumTone(q.eligible)}`}>
                    {quorumLabel(q)}
                  </span>
                  <span className="num text-micro text-ink-500 ml-auto min-w-0 break-words text-right">
                    {m.project}
                    {q.mode === "retrieval" && q.eligible === true && m.verified_at
                      ? ` · verified ${m.verified_at.slice(0, 10)}`
                      : ""}
                  </span>
                </div>
                <div className="text-[13px] text-ink-300 break-words">{m.fact}</div>
                <div className="num text-micro text-ink-500 break-words">
                  {scopeLine(m)}
                  {m.supersedes ? ` · supersedes ${m.supersedes}` : ""}
                </div>
                <div className="text-micro text-ink-500 break-words">{q.reason}</div>
              </button>

              {isOpen && (
                <div className="mt-3 pt-3 border-t border-ink-700">
                  {d?.data ? (
                    <>
                      <div className="text-[13px] text-ink-200 [&_p]:mb-2">
                        <Md text={d.data.body} />
                      </div>
                      <div className="num text-micro text-ink-500 mt-2 break-words">
                        {d.data.author ? `by ${d.data.author} · ` : ""}
                        {d.data.source ? `source ${d.data.source} · ` : ""}
                        {d.data.path}
                      </div>
                      {detailQuorum && (
                        <>
                          <div className="mt-3 flex flex-wrap items-center gap-2 text-[13px]">
                            <span className="slabel">
                              {detailQuorum.mode === "acceptance"
                                ? "server review"
                                : "server availability"}
                            </span>
                            <span className={`chip ${quorumTone(detailQuorum.eligible)}`}>
                              {quorumLabel(detailQuorum)}
                            </span>
                          </div>
                          <p className="mt-1 text-[13px] text-ink-500 break-words">
                            {detailQuorum.reason}
                          </p>
                          <p className="mt-3 text-[13px] text-ink-400">
                            read-only — review and PM finalization require authenticated
                            native agents; browser requests cannot provide that identity.
                          </p>
                          <div className="mt-3 text-[13px] text-ink-500">
                            <span className="slabel mr-2">digest</span>
                            <code className="num block mt-1 break-all text-ink-400">
                              {d.data.revision_digest ?? "unavailable"}
                            </code>
                          </div>
                          {typeof d.data.review_count === "number" && (
                            <div className="num text-micro text-ink-500 mt-2">
                              historical receipts {d.data.review_count}
                            </div>
                          )}
                        </>
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
