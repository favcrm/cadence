import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useWriteBlock } from "../auth/WriteGate";
import Button from "../../ui/Button";
import { IconRefresh, IconWarning } from "../../ui/icons";
import { fmtTime, releaseLabel } from "../../lib/fmt";
import type { UpdateStatus, WaitingTurn } from "../../lib/types";
import "./update.css";
import { useTranslate } from "../../lib/locale";

/** `swe-554 (12m)` — the same rendering the CLI's drain shows. */
// Stored read-error fallback: UI-owned, translated at render (never stored translated).
const UI_READ_FALLBACK = "Could not read the update status.";

export function waiterLabel(w: WaitingTurn): string {
  const age =
    w.age_secs >= 60 ? `${Math.floor(w.age_secs / 60)}m` : `${w.age_secs}s`;
  return `${w.alias} (${age})`;
}

export function waitingLabel(waiting: WaitingTurn[]): string {
  return `waiting for ${waiting.length} turn${waiting.length === 1 ? "" : "s"}: ${waiting.map(waiterLabel).join(", ")}`;
}

function timeLabel(secs: number): string {
  return fmtTime(new Date(secs * 1000).toISOString());
}

function outcome(
  status: UpdateStatus,
): { title: string; text: string; tone: string } | null {
  const result = status.result;
  if (status.error)
    return { title: "Update failed", text: status.error, tone: "fail" };
  if (!result) return null;
  if (result.rolled_back === true)
    return {
      title: "Rolled back",
      text: "The previous release was restored. Review the progress log before trying again.",
      tone: "warn",
    };
  const health = result.health as { ok?: unknown } | null | undefined;
  if (health?.ok === true && result.rolled_back === false)
    return {
      title: "Update complete",
      text: "The new release passed its health check.",
      tone: "ok",
    };
  const check = result.check as { up_to_date?: unknown } | null | undefined;
  if (
    check?.up_to_date === true &&
    result.health === null &&
    result.install === null &&
    result.restart === null &&
    result.rolled_back === false
  )
    return {
      title: "No update needed",
      text: "The run found the release already installed.",
      tone: "ok",
    };
  return {
    title: "Run ended",
    text: "This report does not confirm a healthy update. Review the progress log for details.",
    tone: "warn",
  };
}

/** Release status is observed from the server; a start receipt alone is not a completed run. */
export default function Update({
  viewer,
}: {
  viewer: {
    readOnly: boolean;
    operator: boolean | null;
    /** CAD-1193: splits the `operator: null` unknown into checking vs
     *  a completed-but-unanswered check. */
    access?: "checking" | "unavailable" | null;
    onRetryAccess?: () => void;
  };
}) {
  const t = useTranslate();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [fetching, setFetching] = useState(true);
  const [busy, setBusy] = useState<"check" | "update" | null>(null);
  const [requested, setRequested] = useState(false);
  const [logOpen, setLogOpen] = useState(false);
  const [following, setFollowing] = useState(true);
  const linesRef = useRef<HTMLPreElement>(null);
  const alive = useRef(false);
  const request = useRef(0);
  const reading = useRef(false);
  const action = useRef(false);
  const awaitingRun = useRef(false);
  const follow = useRef(true);
  const blocked = useWriteBlock(viewer.readOnly || !viewer.operator);
  const active = !!(status?.running || status?.pending || requested);
  const checking = busy === "check" || !!status?.checking;

  const load = useCallback(async (poll = false) => {
    if (poll && (reading.current || action.current)) return;
    const id = ++request.current;
    reading.current = true;
    setFetching(true);
    try {
      const next = await api.updateStatus();
      if (!alive.current || id !== request.current) return;
      setStatus(next);
      setReadError(null);
      // A fresh run observation, including a terminal failure, resolves the local receipt.
      if (
        awaitingRun.current &&
        (next.running || next.pending || next.result || next.error)
      ) {
        awaitingRun.current = false;
        setRequested(false);
      }
    } catch (err) {
      if (alive.current && id === request.current)
        setReadError(
          err instanceof ApiError
            ? err.message
            : UI_READ_FALLBACK,
        );
    } finally {
      if (alive.current && id === request.current) {
        reading.current = false;
        setFetching(false);
      }
    }
  }, []);

  useEffect(() => {
    alive.current = true;
    void load();
    return () => {
      alive.current = false;
      ++request.current;
    };
  }, [load]);

  useEffect(() => {
    const timer = window.setInterval(
      () => void load(true),
      active || checking ? 2000 : 30000,
    );
    return () => window.clearInterval(timer);
  }, [load, active, checking]);

  useEffect(() => {
    if (active) setLogOpen(true);
  }, [active]);
  useEffect(() => {
    const node = linesRef.current;
    if (logOpen && node && follow.current) node.scrollTop = node.scrollHeight;
  }, [status?.lines, logOpen]);

  const inactive =
    !!blocked || !status || !!readError || busy !== null || active || checking;
  const run = async (kind: "check" | "update") => {
    if (
      inactive ||
      action.current ||
      (kind === "update" && (!status?.check || status.check.up_to_date))
    )
      return;
    action.current = true;
    ++request.current; // Reads begun before the action cannot restore its old state.
    reading.current = false;
    setBusy(kind);
    setActionError(null);
    try {
      if (kind === "check") await api.updateCheck();
      else {
        const receipt = await api.startUpdate();
        if (!receipt.started)
          throw new Error("The server did not acknowledge an update start.");
        if (alive.current) {
          awaitingRun.current = true;
          setRequested(true);
        }
      }
      if (alive.current) await load();
    } catch (err) {
      if (alive.current)
        setActionError(
          err instanceof Error ? err.message : "The request failed.",
        );
    } finally {
      action.current = false;
      if (alive.current) {
        setBusy(null);
        setFetching(false);
      }
    }
  };

  const report = status?.check;
  const pending = status?.pending;
  const current = status?.current;
  const result =
    status && !active && busy !== "update" ? outcome(status) : null;
  const phase = pending?.phase;
  const phases: Record<string, string> = {
    draining: "Waiting for agent turns to finish",
    restarting: "Restarting on the new release",
    verifying: "Verifying the release",
    installing: "Installing the release",
  };
  const stateTitle =
    busy === "update"
      ? "Starting update…"
      : requested
        ? "Update requested"
        : active
          ? "Update in progress"
          : checking
            ? "Checking for updates…"
            : readError
              ? status
                ? "Last known release status"
                : "Update status unavailable"
              : !status
                ? "Loading update status…"
                : !report
                  ? "Release check unavailable"
                  : report.up_to_date
                    ? "Up to date"
                    : "Update available";
  const stateText =
    busy === "update"
      ? "Waiting for the server to acknowledge the request."
      : requested
        ? "The request was accepted. Waiting for progress from the update process."
        : active
          ? (phases[phase ?? ""] ??
            (phase
              ? "Current phase: {phase}"
              : "The update process is running."))
          : checking
            ? "Finding the latest approved release. The previous check remains below."
            : readError && !status
              ? "Retry the status read to see the installed and available releases."
              : !status
                ? "Reading the installed release and latest check."
                : !report
                  ? "No release check is available yet. Status refreshes automatically."
                  : report.up_to_date
                    ? "The latest approved release is installed."
                    : report.change_count === 1
                      ? "1 change since the installed release."
                      : "{change_count} changes since the installed release.";

  return (
    <main className="update-workspace">
      <header className="update-heading">
        <div>
          <h1>{t("Status")}</h1>
          <p>{t("Board version, release status and the update action.")}</p>
        </div>
        <Button
          icon={<IconRefresh />}
          onClick={() => void load()}
          disabled={busy !== null}
        >{t("Refresh status")}</Button>
      </header>
      {blocked && (
        <p className="update-access">
          {blocked}
          {viewer.operator === null && viewer.access === "unavailable" && viewer.onRetryAccess && (
            // CAD-1193: retry the real access check, not a reload.
            <>
              {" "}
              <Button size="sm" onClick={viewer.onRetryAccess}>{t("Retry access check")}</Button>
            </>
          )}
        </p>
      )}
      {readError && (
        <div className="update-notice" data-tone="fail" role="alert">
          <div>
            <strong>{t("Could not refresh status")}</strong>
            <p>
              {readError === UI_READ_FALLBACK ? t(readError) : readError}
              {" "}{status ? t("Last known information is shown below; refresh before updating.") : t("Try again to read the release status.")}
            </p>
          </div>
          <Button onClick={() => void load()}>{t("Retry status")}</Button>
        </div>
      )}
      {actionError && (
        <div className="update-notice" data-tone="fail" role="alert">
          <div>
            <strong>{t("Request failed")}</strong>
            <p>{actionError}</p>
          </div>
        </div>
      )}
      <section className="update-release">
        <div className="update-state" role="status">
          <span
            className="update-dot"
            data-tone={
              active || checking || busy
                ? "info"
                : !status || !report || readError
                  ? "muted"
                  : report.up_to_date
                    ? "ok"
                    : "info"
            }
          />
          <div>
            <h2>{t(stateTitle)}</h2>
            <p>{t(stateText).replace("{phase}", () => String(phase ?? "")).replace("{change_count}", () => String(report?.change_count ?? ""))}</p>
          </div>
        </div>
        {status && (
          <>
            <dl className="update-versions">
              <div>
                <dt>{t("Installed release")}</dt>
                <dd>
                  {current ? releaseLabel(current.version, current.sha) : t("Installation unavailable")}
                </dd>
              </div>
              <div>
                <dt>{t("Latest approved release")}</dt>
                <dd>
                  {report ? releaseLabel(report.target_version, report.target) : t("Not checked yet")}
                </dd>
              </div>
              <div>
                <dt>{t("Connectivity")}</dt>
                <dd data-state={readError ? "unavailable" : "connected"}>
                  {readError ? t("Last read failed") : t("Connected")}
                </dd>
              </div>
            </dl>
            <div className="update-release-footer">
              <p>
                {status.checked_at ? t("Last checked {time}").replace("{time}", () => timeLabel(status.checked_at!)) : t("No completed check yet")}
                {fetching ? " · " : ""}{fetching ? t("Refreshing status…") : ""}
              </p>
              <div className="update-actions">
                <Button
                  disabled={inactive}
                  loading={checking}
                  onClick={() => void run("check")}
                >
                  {checking ? t("Checking…") : t("Check for updates")}
                </Button>
                <Button
                  variant="primary"
                  disabled={inactive || !report || report.up_to_date}
                  loading={busy === "update" || active}
                  onClick={() => void run("update")}
                >
                  {busy === "update" ? t("Starting…") : requested ? t("Update requested") : active ? t("Updating…") : t("Update")}
                </Button>
              </div>
            </div>
          </>
        )}
      </section>
      {pending && (
        <section className="update-notice" data-tone="info">
          <div>
            <h2>{t("Agent activity")}</h2>
            <p>{t("New turns are paused during this update.")}</p>
            <p>
              {status!.waiting.length
                ? (status!.waiting.length === 1 ? t("waiting for {count} turn: {list}") : t("waiting for {count} turns: {list}")).replace("{count}", String(status!.waiting.length)).replace("{list}", () => status!.waiting.map(waiterLabel).join(", "))
                : t("No active turns reported.")}
            </p>
            <p className="update-secondary">
              {t("Started by {by} · {timeLabel}").replace("{by}", () => String(pending.by ?? "")).replace("{timeLabel}", () => String(timeLabel(pending.since) ?? ""))}</p>
          </div>
        </section>
      )}
      {report && report.blockers.length > 0 && !pending && (
        <section className="update-notice" data-tone="warn">
          <IconWarning size={18} />
          <div>
            <h2>{t("Before updating")}</h2>
            <ul>
              {report.blockers.map((b, i) => (
                <li key={i}>{b}</li>
              ))}
            </ul>
            <p className="update-secondary">{t("Active turns can finish during the drain. The updater checks the release lease before proceeding.")}</p>
          </div>
        </section>
      )}
      {result && (
        <section
          className="update-notice"
          data-tone={result.tone}
          role="status"
        >
          <div>
            <h2>{t(result.title)}</h2>
            <p>{t(result.text)}</p>
          </div>
        </section>
      )}
      {report && (
        <details className="update-disclosure">
          <summary>
            {t("What’s changed")}{" "}
            <span>
              {report.change_count === 1 ? t("{change_count} change").replace("{change_count}", String(report.change_count)) : t("{change_count} changes").replace("{change_count}", String(report.change_count))}
            </span>
          </summary>
          <div className="update-disclosure-body">
            {report.changes.length ? (
              <ul className="update-changes">
                {report.changes.map((c, i) => (
                  <li key={i}>{c}</li>
                ))}
              </ul>
            ) : (
              <p>{t("No change summary was provided by this check.")}</p>
            )}
          </div>
        </details>
      )}
      {status && (active || status.lines.length > 0 || result) && (
        <details
          className="update-disclosure"
          open={logOpen}
          onToggle={(e) => setLogOpen(e.currentTarget.open)}
        >
          <summary>
            {t("Progress log")}{" "}
            <span>{active ? t("Live") : status.lines.length === 1 ? t("{count} line").replace("{count}", "1") : t("{count} lines").replace("{count}", String(status.lines.length))}</span>
          </summary>
          <div className="update-disclosure-body">
            <div className="update-log-heading">
              <p>
                {following ? t("Following new progress") : t("Reading earlier progress")}
              </p>
              <Button
                size="sm"
                onClick={() => {
                  follow.current = true;
                  setFollowing(true);
                  const node = linesRef.current;
                  if (node) node.scrollTop = node.scrollHeight;
                }}
              >{t("Jump to latest")}</Button>
            </div>
            <pre
              ref={linesRef}
              tabIndex={0}
              aria-label={t("Update progress log")}
              onScroll={(e) => {
                const node = e.currentTarget;
                const atEnd =
                  node.scrollHeight - node.clientHeight - node.scrollTop < 24;
                follow.current = atEnd;
                setFollowing(atEnd);
              }}
            >
              {status.lines.length ? status.lines.join("\n") : t("Waiting for progress from the update process…")}
            </pre>
          </div>
        </details>
      )}
      {status && (
        <details className="update-disclosure">
          <summary>{t("Release details")}</summary>
          <dl className="update-disclosure-body update-technical">
            <div>
              <dt>{t("Installed commit")}</dt>
              <dd>
                <code>
                  {current?.sha ?? (current ? t("Not installed") : t("Unavailable"))}
                </code>
              </dd>
            </div>
            {report && (
              <>
                <div>
                  <dt>{t("Target commit")}</dt>
                  <dd>
                    <code>{report.target}</code>
                  </dd>
                </div>
                <div>
                  <dt>{t("Database schema")}</dt>
                  <dd>
                    {report.schema.migration
                      ? t("{current} → {target} · Migration covered by the pre-update backup receipt.").replace("{current}", () => String(report.schema.current ?? t("Unknown"))).replace("{target}", () => String(report.schema.target ?? t("Unknown")))
                      : t("No migration reported · Current {current}, target {target}.").replace("{current}", () => String(report.schema.current ?? t("unknown"))).replace("{target}", () => String(report.schema.target ?? t("unknown")))}
                  </dd>
                </div>
              </>
            )}
            {pending && (
              <div>
                <dt>{t("Pending target")}</dt>
                <dd>
                  <code>{pending.target}</code>
                </dd>
              </div>
            )}
          </dl>
        </details>
      )}
    </main>
  );
}
