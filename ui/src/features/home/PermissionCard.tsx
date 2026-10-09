import { useEffect, useState } from "react";
import { api, ApiError, type MasterPermissionRequest } from "../../lib/api";
import { resources } from "../../lib/resources";

const listeners = new Set<() => void>();

/** The rail and the thread both redraw from the request record. */
export function publishPermission() {
  void resources.overview.invalidate();
  for (const fn of listeners) fn();
}

export function subscribePermission(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export interface PermissionCardData {
  id: string;
  argv: string;
  cwd: string;
  reason: string;
  risk: string;
  prefix: string[] | null;
  status: string;
  decisionLabel: string;
}

/** Past this many characters the reason waits behind "…more" (never cut silently). */
const REASON_CAP = 2000;

/** Invisible, bidi and control characters could reorder or hide the text. */
const INVISIBLE = /[\p{Cf}\p{Default_Ignorable_Code_Point}\p{Cc}]/gu;

/**
 * What Master said about the command: all of it, as text to read before
 * Allow (never refused, never clamped). Only characters that cannot be
 * seen are removed.
 */
export function ReasonBlock({ reason, label = "Master says:" }: { reason: string; label?: string }) {
  const [more, setMore] = useState(false);
  const text = reason.replace(INVISIBLE, (c) => (c === "\n" || c === "\t" ? c : ""));
  const long = text.length > REASON_CAP;
  const shown = long && !more ? text.slice(0, REASON_CAP) : text;
  return (
    <div className="rounded border border-ink-700 bg-ink-900 px-2 py-1.5 min-w-0" data-permission-reason>
      <p className="text-micro text-ink-400">{label}</p>
      <p
        className={`text-label text-ink-300 whitespace-pre-wrap break-words ${more ? "max-h-[50vh] overflow-y-auto" : ""}`}
        tabIndex={more ? 0 : undefined}
        role={more ? "region" : undefined}
        aria-label={more ? "Master's full reason" : undefined}
      >
        {shown}
        {long && !more ? "…" : ""}
      </p>
      {long ? (
        <button type="button" className="lnk text-micro mt-1" aria-expanded={more} onClick={() => setMore((v) => !v)}>
          {more ? "Show less" : "…more"}
        </button>
      ) : null}
    </div>
  );
}

/**
 * The operator's permission prompt (CAD-615), shared by the Needs-you
 * rail and the master thread. A decided request shows its outcome and
 * a second decision is refused by the daemon.
 */
export default function PermissionCard({
  card,
  readOnly,
  onDone,
  compact = false,
}: {
  card: PermissionCardData;
  readOnly: boolean;
  onDone: (text: string) => void;
  /** The To do card (CAD-1216): the whole reason in words and Allow once /
   *  Always / Deny. The command and folder live under Details. */
  compact?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [alwaysOpen, setAlwaysOpen] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const decided = card.status !== "pending";
  const act = compact ? "btn btn-sm" : "lnk text-label";
  const primary = compact ? "btn btn-sm btn-primary" : "lnk text-label";
  const run = (work: () => Promise<unknown>, what: string) => {
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        publishPermission();
        onDone(what);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <div className="mt-2 space-y-2" data-permission={card.id} data-permission-status={card.status}>
      {compact ? null : (
        <>
          <p className="text-micro text-ink-300 break-all">
            <span className="chip bg-ink-800 text-ink-300 mr-1">{card.risk}</span>
            {card.argv}
          </p>
          {card.cwd ? <p className="text-micro text-ink-500 break-all">{card.cwd}</p> : null}
        </>
      )}
      {compact && card.risk !== "low" ? (
        <p className="text-micro text-warn" data-permission-risk={card.risk}>
          {card.risk === "high" ? "High risk" : "Some risk"}: Master wants to run a command that can change or delete things.
        </p>
      ) : null}
      {card.reason ? <ReasonBlock reason={card.reason} /> : null}
      {decided ? (
        <p className="text-micro text-ok" data-permission-decided>
          {card.decisionLabel || card.status}
        </p>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className={primary}
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionAllowOnce(card.id), "Allowed once")}
          >
            Allow once
          </button>
          <button
            type="button"
            className={act}
            disabled={readOnly || busy}
            aria-expanded={alwaysOpen}
            onClick={() => {
              setRejectOpen(false);
              setAlwaysOpen((v) => !v);
            }}
          >
            {compact ? "Always ▾" : "Always allow ▾"}
          </button>
          <button
            type="button"
            className={act}
            disabled={readOnly || busy}
            aria-expanded={rejectOpen}
            onClick={() => {
              setAlwaysOpen(false);
              setRejectOpen((v) => !v);
            }}
          >
            {compact ? "Deny ▾" : "Reject ▾"}
          </button>
        </div>
      )}
      {!decided && alwaysOpen ? (
        <div className="flex flex-wrap gap-2" role="menu" aria-label="always allow scope">
          <button
            type="button"
            className={act}
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionAlways(card.id, "exact", []), "Always: this command")}
          >
            This exact command
          </button>
          {card.prefix ? (
            <button
              type="button"
              className={act}
              disabled={readOnly || busy}
              onClick={() =>
                run(() => api.permissionAlways(card.id, "prefix", card.prefix ?? []), "Always: prefix")
              }
            >
              Prefix {card.prefix.join(" ")}
            </button>
          ) : null}
        </div>
      ) : null}
      {!decided && rejectOpen ? (
        <div className="flex flex-wrap gap-2" role="menu" aria-label="reject">
          <button
            type="button"
            className={act}
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionReject(card.id, false), "Rejected")}
          >
            Reject once
          </button>
          <button
            type="button"
            className={act}
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionReject(card.id, true), "Rejected — won't ask again")}
          >
            Don't ask again
          </button>
        </div>
      ) : null}
      {error ? (
        <p className="text-micro text-fail break-words" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

export function permissionIdFromText(text: string): string | null {
  // The daemon writes `Permission requested (<id>, risk <r>): ...`; the bare `(<id>)` form still parses.
  const m = text.match(/Permission requested \(([A-Za-z0-9-]+)[,)]/);
  return m ? m[1] : null;
}

function cardFromRequest(req: MasterPermissionRequest): PermissionCardData {
  return {
    id: req.id,
    argv: req.command,
    cwd: req.cwd,
    reason: req.reason,
    risk: req.risk,
    prefix: req.prefix,
    status: req.status,
    decisionLabel: req.decision_label ?? "",
  };
}

/** The same card, fed by the master thread's permission message. */
export function ThreadPermission({ text, readOnly }: { text: string; readOnly: boolean }) {
  const id = permissionIdFromText(text);
  const [card, setCard] = useState<PermissionCardData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => {
    if (!id) return;
    api
      .permissionRules()
      .then((out) => {
        const req = out.requests.find((r) => r.id === id);
        setCard(req ? cardFromRequest(req) : null);
      })
      .catch((e: ApiError) => setError(e.message ?? String(e)));
  };
  useEffect(() => {
    load();
    return subscribePermission(load);
  }, [id]);
  if (!id) return <p className="text-micro text-ink-400 whitespace-pre-wrap break-words">{text}</p>;
  return (
    <div className="card px-3 py-2 ml-8 min-w-0" data-permission-thread={id}>
      {card ? (
        <PermissionCard card={card} readOnly={readOnly} onDone={() => load()} />
      ) : (
        <p className="text-micro text-ink-400 whitespace-pre-wrap break-words">{text}</p>
      )}
      {error ? (
        <p className="text-micro text-fail" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
