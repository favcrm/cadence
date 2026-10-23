import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslate } from "../../lib/locale";
import "./decisions.css";

/**
 * CAD-1784: owner decisions for the tenant Home assistant.
 *
 * The tenant Worker exposes the staged writes as `/api/assist/actions`
 * (omission selects Home) and owner-only approve/deny routes. Other boards
 * answer 404/501 there, so the trigger simply does not render. Nothing here
 * invents a decision: the server owns the owner gate and idempotency.
 */

type StagedAction = {
  id: string;
  tool: string;
  input: Record<string, unknown>;
  createdAt: number;
  status: "pending" | "approved" | "denied";
  result?: {
    ok?: boolean;
    ticket?: { id?: string };
  };
};

type Row = {
  id: string;
  title: string;
  project: string;
  priority: string;
  body: string;
  age: string;
  status: "pending" | "approved" | "denied";
  receipt: string | null;
  unconfirmed: boolean;
};

type LoadState =
  | { kind: "loading" }
  | { kind: "unavailable" }
  | { kind: "ready"; rows: Row[] };

class DecisionsError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

function timeAgo(createdAt: number): string {
  const minutes = Math.max(0, Math.floor((Date.now() - createdAt) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours}h ago` : `${Math.floor(hours / 24)}d ago`;
}

function readString(input: Record<string, unknown>, key: string): string {
  const value = input[key];
  return typeof value === "string" ? value : "";
}

function toRow(action: StagedAction): Row | null {
  if (action.tool !== "board_create_issue") return null;
  const title = readString(action.input, "title").trim();
  if (!title) return null;
  const receipt =
    action.status === "approved" && typeof action.result?.ticket?.id === "string"
      ? action.result.ticket.id
      : null;
  return {
    id: action.id,
    title,
    project: readString(action.input, "project"),
    priority: readString(action.input, "priority"),
    body: readString(action.input, "body"),
    age: timeAgo(action.createdAt),
    status: action.status,
    receipt,
    unconfirmed: false,
  };
}

async function readActions(signal: AbortSignal): Promise<StagedAction[]> {
  const response = await fetch("/api/assist/actions", {
    credentials: "same-origin",
    headers: { accept: "application/json" },
    signal,
  });
  if (response.status === 404 || response.status === 501) throw new DecisionsError(404, "unavailable", "unavailable");
  const body = (await response.json().catch(() => null)) as
    | { actions?: unknown; error?: { code?: string; message?: string } }
    | null;
  if (!response.ok)
    throw new DecisionsError(
      response.status,
      body?.error?.code ?? `http_${response.status}`,
      body?.error?.message ?? "The decisions list could not be read.",
    );
  if (!Array.isArray(body?.actions)) throw new DecisionsError(502, "invalid_response", "The decisions list was invalid.");
  return body.actions.filter(
    (value): value is StagedAction =>
      !!value &&
      typeof value === "object" &&
      typeof (value as StagedAction).id === "string" &&
      typeof (value as StagedAction).tool === "string" &&
      typeof (value as StagedAction).createdAt === "number" &&
      ["pending", "approved", "denied"].includes((value as StagedAction).status),
  );
}

async function decide(id: string, decision: "approve" | "deny"): Promise<void> {
  const response = await fetch(`/api/assist/actions/${encodeURIComponent(id)}/${decision}`, {
    method: "POST",
    credentials: "same-origin",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: "{}",
  });
  const body = (await response.json().catch(() => null)) as { error?: { code?: string; message?: string } } | null;
  if (response.ok) return;
  throw new DecisionsError(
    response.status,
    body?.error?.code ?? `http_${response.status}`,
    body?.error?.message ?? "The decision was refused.",
  );
}

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      aria-hidden="true"
      className="decisions-chevron"
      data-open={open || undefined}
    >
      <path d="m6 3.5 4.5 4.5L6 12.5" />
    </svg>
  );
}

export default function DecisionsPanel() {
  const t = useTranslate();
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<"waiting" | "decided">("waiting");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState<{ id: string; decision: "approve" | "deny" } | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "warn" | "err"; text: string } | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const drawer = useRef<HTMLDivElement>(null);

  const load = useCallback(async (signal?: AbortSignal) => {
    const controller = new AbortController();
    const stop = signal ? () => controller.abort() : () => {};
    signal?.addEventListener("abort", stop, { once: true });
    try {
      const actions = await readActions(controller.signal);
      const rows = actions.map(toRow).filter((row): row is Row => row !== null);
      setState({ kind: "ready", rows });
    } catch (error) {
      if (controller.signal.aborted) return;
      if (error instanceof DecisionsError && (error.status === 404 || error.status === 501 || error.status === 401 || error.status === 403)) {
        setState({ kind: "unavailable" });
        return;
      }
      // Keep the last good list; a refresh failure is not a reason to invent
      // an empty queue.
      setState((current) => (current.kind === "ready" ? current : { kind: "unavailable" }));
    } finally {
      signal?.removeEventListener("abort", stop);
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  const waiting = state.kind === "ready" ? state.rows.filter((row) => row.status === "pending") : [];
  const decided = state.kind === "ready" ? state.rows.filter((row) => row.status !== "pending") : [];

  useEffect(() => {
    if (state.kind !== "ready" || waiting.length === 0) return;
    const timer = window.setInterval(() => void load(), 30_000);
    return () => window.clearInterval(timer);
  }, [load, state.kind, waiting.length]);

  useEffect(() => {
    document.body.toggleAttribute("data-decisions-open", open);
    return () => document.body.removeAttribute("data-decisions-open");
  }, [open]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 4_200);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (expanded !== null) {
        setExpanded(null);
        return;
      }
      setOpen(false);
      trigger.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [expanded, open]);

  const close = () => {
    setOpen(false);
    setExpanded(null);
    trigger.current?.focus();
  };

  const run = async (row: Row, decision: "approve" | "deny") => {
    if (busy !== null) return;
    setBusy({ id: row.id, decision });
    setConflict(null);
    try {
      await decide(row.id, decision);
      setNotice({
        kind: "ok",
        text: decision === "approve" ? t("Issue created · cannot repeat") : t("Denied · nothing created"),
      });
      setExpanded(null);
      await load();
      window.setTimeout(() => {
        drawer.current?.querySelector<HTMLButtonElement>(".decisions-row")?.focus();
      }, 0);
    } catch (error) {
      if (error instanceof DecisionsError && error.status === 403) {
        setConflict(t("Only the workspace owner can decide this request."));
      } else if (error instanceof DecisionsError && error.status === 409) {
        setConflict(t("This request was already decided. The list is refreshing."));
        await load();
      } else if (error instanceof DecisionsError && error.status === 404) {
        setConflict(t("This request no longer exists."));
        await load();
      } else {
        setState((current) =>
          current.kind === "ready"
            ? {
                kind: "ready",
                rows: current.rows.map((entry) =>
                  entry.id === row.id ? { ...entry, unconfirmed: true } : entry,
                ),
              }
            : current,
        );
        setConflict(t("The outcome is unconfirmed. Retry the same decision; it cannot run twice."));
      }
    } finally {
      setBusy(null);
    }
  };

  if (state.kind === "loading" || state.kind === "unavailable") return null;
  if (waiting.length === 0 && !open) return null;

  const rows = tab === "waiting" ? waiting : decided;

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="decisions-trigger"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => (open ? close() : setOpen(true))}
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
          <path d="M8 1.8 14 5v6L8 14.2 2 11V5z" />
          <path d="m8 6.2 2.4 1.4v2.8L8 11.8 5.6 10.4V7.6z" />
        </svg>
        <span className="decisions-trigger-label">{t("Decisions")}</span>
        <span className="decisions-count" aria-live="polite">
          {waiting.length}
        </span>
      </button>

      {open && typeof document !== "undefined" && createPortal(
        <div ref={drawer} className="decisions-drawer" role="dialog" aria-modal="false" aria-labelledby="decisions-title">
          <header className="decisions-head">
            <div>
              <h2 id="decisions-title">{t("Decisions")}</h2>
              <p>{t("The assistant can ask; only you can approve.")}</p>
            </div>
            <button type="button" className="decisions-close" aria-label={t("Close decisions")} onClick={close}>
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
                <path d="m3.5 3.5 9 9m0-9-9 9" />
              </svg>
            </button>
          </header>

          <div className="decisions-tabs" role="tablist" aria-label={t("Decision state")}>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "waiting"}
              onClick={() => {
                setTab("waiting");
                setExpanded(null);
              }}
            >
              {t("Waiting")} <span>{waiting.length}</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "decided"}
              onClick={() => {
                setTab("decided");
                setExpanded(null);
              }}
            >
              {t("Decided")} <span>{decided.length}</span>
            </button>
          </div>

          <div className="decisions-list" role="tabpanel">
            {rows.length === 0 ? (
              <p className="decisions-empty">
                {t("No decisions waiting. Requests appear here when the assistant stages a write.")}
              </p>
            ) : (
              rows.map((row) => {
                const isOpen = expanded === row.id;
                return (
                  <article
                    key={row.id}
                    className="decisions-card"
                    data-state={row.unconfirmed ? "unconfirmed" : row.status}
                    data-busy={busy?.id === row.id || undefined}
                  >
                    <button
                      type="button"
                      className="decisions-row"
                      aria-expanded={isOpen}
                      onClick={() => { setExpanded(isOpen ? null : row.id); setConflict(null); }}
                    >
                      <span className="decisions-row-main">
                        <span className="decisions-row-title">{row.title}</span>
                        <span className="decisions-row-meta">
                          {row.status === "pending"
                            ? t("create issue")
                            : row.status === "approved"
                              ? t("created")
                              : t("denied")}
                          {row.project ? ` · ${row.project}` : ""}
                          {row.priority ? ` · ${row.priority}` : ""}
                          {row.unconfirmed ? ` · ${t("unconfirmed")}` : ""}
                        </span>
                      </span>
                      <span className="decisions-row-side">
                        <span className="decisions-age">{row.age}</span>
                        <Chevron open={isOpen} />
                      </span>
                    </button>

                    {isOpen && (
                      <div className="decisions-body">
                        {row.status === "pending" && (
                          <p className="decisions-effect">{t("Creates one issue · nothing sent")}</p>
                        )}
                        {row.body && <p className="decisions-rationale">{row.body}</p>}
                        {row.status === "approved" && row.receipt && (
                          <p className="decisions-effect">{t("One issue created · one-time")}</p>
                        )}
                        {row.status === "pending" && conflict && (
                          <p className="decisions-error" role="alert">{conflict}</p>
                        )}
                        {row.status === "pending" && (
                          <div className="decisions-actions">
                            <button
                              type="button"
                              className="decisions-btn decisions-btn-primary"
                              disabled={busy !== null}
                              aria-busy={(busy?.id === row.id && busy.decision === "approve") || undefined}
                              onClick={() => void run(row, "approve")}
                            >
                              {busy?.id === row.id && busy.decision === "approve" ? (
                                <>
                                  <span className="decisions-spin" aria-hidden="true" />
                                  {t("Approving…")}
                                </>
                              ) : row.unconfirmed ? (
                                t("Retry")
                              ) : (
                                t("Approve")
                              )}
                            </button>
                            <button
                              type="button"
                              className="decisions-btn decisions-btn-ghost"
                              disabled={busy !== null}
                              aria-busy={(busy?.id === row.id && busy.decision === "deny") || undefined}
                              onClick={() => void run(row, "deny")}
                            >
                              {busy?.id === row.id && busy.decision === "deny" ? (
                                <>
                                  <span className="decisions-spin" aria-hidden="true" />
                                  {t("Denying…")}
                                </>
                              ) : (
                                t("Deny")
                              )}
                            </button>
                          </div>
                        )}
                      </div>
                    )}
                  </article>
                );
              })
            )}
          </div>

          <footer className="decisions-foot">
            <span>{t("Owner only")}</span>
            <span>{t("Home")}</span>
          </footer>

          {notice && (
            <div className={`decisions-toast decisions-toast-${notice.kind}`} role="status">
              <span aria-hidden="true" className="decisions-dot" />
              {notice.text}
            </div>
          )}
        </div>,
        document.body,
      )}
    </>
  );
}
