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

/**
 * The operator's permission prompt (CAD-615), shared by the Needs-you
 * rail and the master thread. A decided request shows its outcome and
 * a second decision is refused by the daemon.
 */
export default function PermissionCard({
  card,
  readOnly,
  onDone,
}: {
  card: PermissionCardData;
  readOnly: boolean;
  onDone: (text: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [alwaysOpen, setAlwaysOpen] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const decided = card.status !== "pending";
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
      <p className="text-micro text-ink-300 break-all">
        <span className="chip bg-ink-800 text-ink-300 mr-1">{card.risk}</span>
        {card.argv}
      </p>
      {card.cwd ? <p className="text-micro text-ink-500 break-all">{card.cwd}</p> : null}
      {card.reason ? <p className="text-micro text-ink-500">{card.reason}</p> : null}
      {decided ? (
        <p className="text-micro text-ok" data-permission-decided>
          {card.decisionLabel || card.status}
        </p>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="lnk text-label"
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionAllowOnce(card.id), "Allowed once")}
          >
            Allow once
          </button>
          <button
            type="button"
            className="lnk text-label"
            disabled={readOnly || busy}
            aria-expanded={alwaysOpen}
            onClick={() => {
              setRejectOpen(false);
              setAlwaysOpen((v) => !v);
            }}
          >
            Always allow ▾
          </button>
          <button
            type="button"
            className="lnk text-label"
            disabled={readOnly || busy}
            aria-expanded={rejectOpen}
            onClick={() => {
              setAlwaysOpen(false);
              setRejectOpen((v) => !v);
            }}
          >
            Reject ▾
          </button>
        </div>
      )}
      {!decided && alwaysOpen ? (
        <div className="flex flex-wrap gap-2" role="menu" aria-label="always allow scope">
          <button
            type="button"
            className="lnk text-label"
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionAlways(card.id, "exact", []), "Always: this command")}
          >
            This exact command
          </button>
          {card.prefix ? (
            <button
              type="button"
              className="lnk text-label"
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
            className="lnk text-label"
            disabled={readOnly || busy}
            onClick={() => run(() => api.permissionReject(card.id, false), "Rejected")}
          >
            Reject once
          </button>
          <button
            type="button"
            className="lnk text-label"
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
  const m = text.match(/Permission requested \(([A-Za-z0-9-]+)\)/);
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
  if (!id) return <p className="text-micro text-ink-400 whitespace-pre-wrap">{text}</p>;
  return (
    <div className="card px-3 py-2 ml-8 min-w-0" data-permission-thread={id}>
      {card ? (
        <PermissionCard card={card} readOnly={readOnly} onDone={() => load()} />
      ) : (
        <p className="text-micro text-ink-400 whitespace-pre-wrap">{text}</p>
      )}
      {error ? (
        <p className="text-micro text-fail" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
