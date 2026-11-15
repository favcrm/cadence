import { createContext, useContext, useEffect, useState } from "react";
import { api, ApiError, type MasterPermissionRequest } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import { subscribeMasterChanged } from "../../lib/masterChanged";
import { useWriteBlock, WriteGate } from "../auth/WriteGate";
import { cleanPermissionInline, cleanPermissionReason } from "./permissionFormat";

/**
 * Provides a composer prefill callback for `Reject & reply…` across nested
 * `PermissionCard` surfaces (`ThreadPermission`, `TodoCard`, and `/permissions`)
 * without prop-drilling.
 */
export const PermissionReplyContext = createContext<((argv: string) => void) | null>(null);

/** Gates legacy in-thread `ThreadPermission` rule fetches on non-hosted boards. */
export const PermissionEnabledContext = createContext<boolean>(true);

/** Formats the steering prefix seeded into the chat composer on reject-and-reply. */
export function rejectReplyPrompt(command: string): string {
  return command.includes("`") ? `Instead of "${command}": ` : `Instead of \`${command}\`: `;
}

/** Appends a reject-and-reply prompt while preserving any existing unsent text. */
export function appendRejectReplyDraft(existing: string, command: string): string {
  const prompt = rejectReplyPrompt(command);
  return existing.trim() ? `${existing.trimEnd()}\n${prompt}` : prompt;
}

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

/**
 * What Master said about the command: all of it, as text to read before
 * Allow (never refused, never clamped). Only characters that cannot be
 * seen are removed.
 */
export function ReasonBlock({ reason, label = "Master says:" }: { reason: string; label?: string }) {
  const t = useTranslate();
  const [more, setMore] = useState(false);
  const text = cleanPermissionReason(reason);
  const long = text.length > REASON_CAP;
  const shown = long && !more ? text.slice(0, REASON_CAP) : text;
  return (
    <div className="rounded border border-ink-700 bg-ink-900 px-2 py-1.5 min-w-0" data-permission-reason>
      <p className="text-micro text-ink-400">{t(label)}</p>
      <p
        className={`text-label text-ink-300 whitespace-pre-wrap break-words ${more ? "max-h-[50vh] overflow-y-auto" : ""}`}
        tabIndex={more ? 0 : undefined}
        role={more ? "region" : undefined}
        aria-label={more ? t("Master's full reason") : undefined}
      >
        <bdi>{shown}</bdi>
        {long && !more ? "…" : ""}
      </p>
      {long ? (
        <button type="button" className="lnk text-micro mt-1" aria-expanded={more} onClick={() => setMore((v) => !v)}>
          {more ? t("Show less") : t("…more")}
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
  onRejectAndReply,
}: {
  card: PermissionCardData;
  readOnly: boolean;
  onDone: (text: string) => void;
  /** The To do card (CAD-1216): the whole reason in words and Allow once /
   *  Always / Deny. The command and folder live under Details. */
  compact?: boolean;
  onRejectAndReply?: (argv: string) => void;
}) {
  const t = useTranslate();
  const gateBlock = useContext(WriteGate);
  const writeBlock = useWriteBlock(readOnly) ?? gateBlock;
  const blocked = readOnly || writeBlock !== null;
  const ctxReply = useContext(PermissionReplyContext);
  const reply = !blocked ? (onRejectAndReply ?? ctxReply) : null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [alwaysOpen, setAlwaysOpen] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const decided = card.status !== "pending";
  const act = compact ? "btn btn-sm" : "lnk text-label";
  const primary = compact ? "btn btn-sm btn-primary" : "lnk text-label";
  const run = (work: () => Promise<unknown>, what: string, after?: () => void) => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        publishPermission();
        after?.();
        onDone(what);
      })
      .catch((e: unknown) => {
        const msg = e instanceof ApiError ? e.message : e instanceof Error ? e.message : String(e);
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
          setAlwaysOpen(false);
          setRejectOpen(false);
          setError(msg);
          publishPermission();
          return;
        }
        setError(msg);
      })
      .finally(() => setBusy(false));
  };
  return (
    <div className="mt-2 space-y-2" data-permission={card.id} data-permission-status={card.status}>
      {compact ? null : (
        <>
          <p className="text-micro text-ink-300 break-all">
            <span className="chip bg-ink-800 text-ink-300 mr-1">{t(card.risk)}</span>
            <bdi>{cleanPermissionInline(card.argv)}</bdi>
          </p>
          {card.cwd ? (
            <p className="text-micro text-ink-500 break-all">
              <bdi>{cleanPermissionInline(card.cwd)}</bdi>
            </p>
          ) : null}
        </>
      )}
      {compact && card.risk !== "low" ? (
        <p className="text-micro text-warn" data-permission-risk={card.risk}>
          {card.risk === "high" ? t("High risk") : t("Some risk")}: {t("Master wants to run a command that can change or delete things.")}
        </p>
      ) : null}
      {card.reason ? <ReasonBlock reason={card.reason} /> : null}
      {decided ? (
        <p className="text-micro text-ok" data-permission-decided>
          <bdi>{cleanPermissionInline(t(card.decisionLabel || card.status))}</bdi>
        </p>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className={primary}
            disabled={blocked || busy}
            onClick={() => run(() => api.permissionAllowOnce(card.id), "Allowed once")}
          >
            {t("Allow once")}
          </button>
          <button
            type="button"
            className={act}
            disabled={blocked || busy}
            aria-expanded={alwaysOpen}
            onClick={() => {
              setRejectOpen(false);
              setAlwaysOpen((v) => !v);
            }}
          >
            {compact ? t("Always ▾") : t("Always allow ▾")}
          </button>
          <button
            type="button"
            className={act}
            disabled={blocked || busy}
            aria-expanded={rejectOpen}
            onClick={() => {
              setAlwaysOpen(false);
              setRejectOpen((v) => !v);
            }}
          >
            {compact ? t("Deny ▾") : t("Reject ▾")}
          </button>
        </div>
      )}
      {!decided && alwaysOpen ? (
        <div className="flex flex-wrap gap-2" role="menu" aria-label={t("always allow scope")}>
          <button
            type="button"
            className={act}
            disabled={blocked || busy}
            onClick={() => run(() => api.permissionAlways(card.id, "exact", []), "Always: this command")}
          >
            {t("This exact command")}
          </button>
          {card.prefix ? (
            <button
              type="button"
              className={act}
              disabled={blocked || busy}
              onClick={() =>
                run(() => api.permissionAlways(card.id, "prefix", card.prefix ?? []), "Always: prefix")
              }
            >
              {t("Prefix")} <bdi>{cleanPermissionInline(card.prefix.join(" "))}</bdi>
            </button>
          ) : null}
        </div>
      ) : null}
      {!decided && rejectOpen ? (
        <div className="flex flex-wrap gap-2" role="menu" aria-label={t("reject")}>
          <button
            type="button"
            className={act}
            disabled={blocked || busy}
            onClick={() => run(() => api.permissionReject(card.id, false), "Rejected")}
          >
            {t("Reject once")}
          </button>
          {reply ? (
            <button
              type="button"
              className={act}
              disabled={blocked || busy}
              data-permission-reject-reply
              onClick={() => run(() => api.permissionReject(card.id, false), "Rejected", () => reply(card.argv))}
            >
              {t("Reject & reply…")}
            </button>
          ) : null}
          <button
            type="button"
            className={act}
            disabled={blocked || busy}
            onClick={() => run(() => api.permissionReject(card.id, true), "Rejected — won't ask again")}
          >
            {t("Don't ask again")}
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

export function cardFromRequest(req: MasterPermissionRequest): PermissionCardData {
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
export function ThreadPermission({
  text,
  readOnly,
  enabled,
}: {
  text: string;
  readOnly: boolean;
  enabled?: boolean;
}) {
  const id = permissionIdFromText(text);
  const ctxEnabled = useContext(PermissionEnabledContext);
  const masterSnap = resources.masterState.get().data;
  const masterHosted = masterSnap !== null && Object.hasOwn(masterSnap, "actor_identity");
  const canRead = (enabled ?? ctxEnabled) && !masterHosted;
  const [card, setCard] = useState<PermissionCardData | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    let revision = 0;
    setCard(null);
    setError(null);
    if (!id || !canRead) return;
    const load = () => {
      const current = ++revision;
      void api.permissionRules().then((out) => {
        if (!alive || current !== revision) return;
        const req = out.requests.find((r) => r.id === id);
        setCard(req ? cardFromRequest(req) : null);
        setError(null);
      }).catch((e: ApiError) => {
        if (!alive || current !== revision) return;
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) setCard(null);
        setError(e.message ?? String(e));
      });
    };
    load();
    const offLocal = subscribePermission(load);
    const offMaster = subscribeMasterChanged(load);
    return () => {
      alive = false;
      offLocal();
      offMaster();
    };
  }, [id, canRead]);
  const fallbackText = cleanPermissionReason(text);
  if (!id || !canRead) {
    return (
      <p className="text-micro text-ink-400 whitespace-pre-wrap break-words" data-permission-thread={id ?? undefined}>
        <bdi>{fallbackText}</bdi>
      </p>
    );
  }
  return (
    <div className="card px-3 py-2 ml-8 min-w-0" data-permission-thread={id}>
      {card ? (
        <PermissionCard card={card} readOnly={readOnly} onDone={() => setError(null)} compact />
      ) : (
        <p className="text-micro text-ink-400 whitespace-pre-wrap break-words">
          <bdi>{fallbackText}</bdi>
        </p>
      )}
      {error ? (
        <p className="text-micro text-fail" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
