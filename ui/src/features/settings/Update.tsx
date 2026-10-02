import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useWriteBlock } from "../auth/WriteGate";
import Button from "../../ui/Button";
import { IconRefresh, IconWarning } from "../../ui/icons";
import { fmtTime, releaseLabel } from "../../lib/fmt";
import type { UpdateStatus, WaitingTurn } from "../../lib/types";
import "./update.css";

/** `swe-554 (12m)` — the same rendering the CLI's drain shows. */
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
  viewer: { readOnly: boolean; operator: boolean };
}) {
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
            : "Could not read the update status.",
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
              ? `Current phase: ${phase}`
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
                    : `${report.change_count} change${report.change_count === 1 ? "" : "s"} since the installed release.`;

  return (
    <main className="update-workspace">
      <header className="update-heading">
        <div>
          <h1>Update</h1>
          <p>Review the latest release and follow its installation.</p>
        </div>
        <Button
          icon={<IconRefresh />}
          onClick={() => void load()}
          disabled={busy !== null}
        >
          Refresh status
        </Button>
      </header>
      {blocked && <p className="update-access">{blocked}</p>}
      {readError && (
        <div className="update-notice" data-tone="fail" role="alert">
          <div>
            <strong>Could not refresh status</strong>
            <p>
              {readError}
              {status
                ? " Last known information is shown below; refresh before updating."
                : " Try again to read the release status."}
            </p>
          </div>
          <Button onClick={() => void load()}>Retry status</Button>
        </div>
      )}
      {actionError && (
        <div className="update-notice" data-tone="fail" role="alert">
          <div>
            <strong>Request failed</strong>
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
            <h2>{stateTitle}</h2>
            <p>{stateText}</p>
          </div>
        </div>
        {status && (
          <>
            <dl className="update-versions">
              <div>
                <dt>Installed release</dt>
                <dd>
                  {current
                    ? releaseLabel(current.version, current.sha)
                    : "Installation unavailable"}
                </dd>
              </div>
              <div>
                <dt>Latest approved release</dt>
                <dd>
                  {report
                    ? releaseLabel(report.target_version, report.target)
                    : "Not checked yet"}
                </dd>
              </div>
            </dl>
            <div className="update-release-footer">
              <p>
                {status.checked_at
                  ? `Last checked ${timeLabel(status.checked_at)}`
                  : "No completed check yet"}
                {fetching ? " · Refreshing status…" : ""}
              </p>
              <div className="update-actions">
                <Button
                  disabled={inactive}
                  loading={checking}
                  onClick={() => void run("check")}
                >
                  {checking ? "Checking…" : "Check for updates"}
                </Button>
                <Button
                  variant="primary"
                  disabled={inactive || !report || report.up_to_date}
                  loading={busy === "update" || active}
                  onClick={() => void run("update")}
                >
                  {busy === "update"
                    ? "Starting…"
                    : requested
                      ? "Update requested"
                      : active
                        ? "Updating…"
                        : "Update"}
                </Button>
              </div>
            </div>
          </>
        )}
      </section>
      {pending && (
        <section className="update-notice" data-tone="info">
          <div>
            <h2>Agent activity</h2>
            <p>New turns are paused during this update.</p>
            <p>
              {status!.waiting.length
                ? waitingLabel(status!.waiting)
                : "No active turns reported."}
            </p>
            <p className="update-secondary">
              Started by {pending.by} · {timeLabel(pending.since)}
            </p>
          </div>
        </section>
      )}
      {report && report.blockers.length > 0 && !pending && (
        <section className="update-notice" data-tone="warn">
          <IconWarning size={18} />
          <div>
            <h2>Before updating</h2>
            <ul>
              {report.blockers.map((b, i) => (
                <li key={i}>{b}</li>
              ))}
            </ul>
            <p className="update-secondary">
              Active turns can finish during the drain. The updater checks the
              release lease before proceeding.
            </p>
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
            <h2>{result.title}</h2>
            <p>{result.text}</p>
          </div>
        </section>
      )}
      {report && (
        <details className="update-disclosure">
          <summary>
            What’s changed{" "}
            <span>
              {report.change_count} change{report.change_count === 1 ? "" : "s"}
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
              <p>No change summary was provided by this check.</p>
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
            Progress log{" "}
            <span>{active ? "Live" : `${status.lines.length} lines`}</span>
          </summary>
          <div className="update-disclosure-body">
            <div className="update-log-heading">
              <p>
                {following
                  ? "Following new progress"
                  : "Reading earlier progress"}
              </p>
              <Button
                size="sm"
                onClick={() => {
                  follow.current = true;
                  setFollowing(true);
                  const node = linesRef.current;
                  if (node) node.scrollTop = node.scrollHeight;
                }}
              >
                Jump to latest
              </Button>
            </div>
            <pre
              ref={linesRef}
              tabIndex={0}
              aria-label="Update progress log"
              onScroll={(e) => {
                const node = e.currentTarget;
                const atEnd =
                  node.scrollHeight - node.clientHeight - node.scrollTop < 24;
                follow.current = atEnd;
                setFollowing(atEnd);
              }}
            >
              {status.lines.length
                ? status.lines.join("\n")
                : "Waiting for progress from the update process…"}
            </pre>
          </div>
        </details>
      )}
      {status && (
        <details className="update-disclosure">
          <summary>Release details</summary>
          <dl className="update-disclosure-body update-technical">
            <div>
              <dt>Installed commit</dt>
              <dd>
                <code>
                  {current?.sha ?? (current ? "Not installed" : "Unavailable")}
                </code>
              </dd>
            </div>
            {report && (
              <>
                <div>
                  <dt>Target commit</dt>
                  <dd>
                    <code>{report.target}</code>
                  </dd>
                </div>
                <div>
                  <dt>Database schema</dt>
                  <dd>
                    {report.schema.migration
                      ? `${report.schema.current ?? "Unknown"} → ${report.schema.target ?? "Unknown"} · Migration covered by the pre-update backup receipt.`
                      : `No migration reported · Current ${report.schema.current ?? "unknown"}, target ${report.schema.target ?? "unknown"}.`}
                  </dd>
                </div>
              </>
            )}
            {pending && (
              <div>
                <dt>Pending target</dt>
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
