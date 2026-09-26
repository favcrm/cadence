import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useWriteBlock } from "../auth/WriteGate";
import Button from "../../ui/Button";
import { fmtTime } from "../../lib/fmt";
import type { UpdateStatus, WaitingTurn } from "../../lib/types";

/** `swe-554 (12m)` — the same rendering the CLI's drain shows. */
export function waiterLabel(w: WaitingTurn): string {
  const age = w.age_secs >= 60 ? `${Math.floor(w.age_secs / 60)}m` : `${w.age_secs}s`;
  return `${w.alias} (${age})`;
}

export function waitingLabel(waiting: WaitingTurn[]): string {
  const list = waiting.map(waiterLabel).join(", ");
  return `waiting for ${waiting.length} turn${waiting.length === 1 ? "" : "s"}: ${list}`;
}

/**
 * CAD-561: Settings → Update. Shows the current version and, from the
 * last check, "Update available · N changes" with the summary; the
 * Update button (operator-only on the server) runs the same pipeline as
 * `cadence update` and streams its progress. Polls faster while an
 * update runs.
 */
export default function Update({ viewer }: { viewer: { readOnly: boolean; operator: boolean } }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"check" | "update" | null>(null);
  const linesRef = useRef<HTMLPreElement | null>(null);
  const blocked = useWriteBlock(!viewer.operator);
  const running = status?.running ?? false;

  const load = useCallback(async () => {
    try {
      setStatus(await api.updateStatus());
      setError(null);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Could not read the update status.");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const every = running ? 2000 : 30000;
    const timer = window.setInterval(() => void load(), every);
    return () => window.clearInterval(timer);
  }, [load, running]);

  useEffect(() => {
    const node = linesRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [status?.lines.length]);

  const runCheck = async () => {
    setBusy("check");
    try {
      await api.updateCheck();
      await load();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The check failed.");
    } finally {
      setBusy(null);
    }
  };

  const start = async () => {
    setBusy("update");
    try {
      await api.startUpdate();
      await load();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The update could not start.");
    } finally {
      setBusy(null);
    }
  };

  const current = status?.current;
  const report = status?.check ?? null;
  const pending = status?.pending ?? null;
  const waiting = status?.waiting ?? [];

  return (
    <section className="mt-6 space-y-4">
      {pending && (
        <div className="rounded border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-body">
          <div className="font-semibold text-ink-100">
            An update is {pending.phase} — the fleet is not starting new turns.
          </div>
          <div className="mt-1 text-ink-300">
            {pending.from ?? "?"} → {pending.target} by {pending.by} since{" "}
            <span className="num">{fmtTime(new Date(pending.since * 1000).toISOString())}</span>
            {waiting.length > 0 ? ` · ${waitingLabel(waiting)}` : " · nothing in flight"}
          </div>
        </div>
      )}

      <div className="rounded border border-ink-700 px-4 py-4">
        <div className="flex items-baseline justify-between gap-4">
          <div>
            <div className="text-section font-semibold text-ink-100">Update</div>
            <div className="mt-1 text-body text-ink-300">
              Current:{" "}
              <span className="num">
                {current?.version ?? current?.sha ?? "no cadence link installed"}
              </span>
            </div>
          </div>
          <div className="flex gap-2">
            <Button
              onClick={() => void runCheck()}
              disabled={busy !== null || running}
              loading={busy === "check" || status?.checking}
              title={blocked ?? undefined}
            >
              {busy === "check" || status?.checking ? "Checking…" : "Check for updates"}
            </Button>
            <Button
              variant="primary"
              onClick={() => void start()}
              disabled={busy !== null || running || !viewer.operator || report === null || report.up_to_date}
              loading={running}
              title={blocked ?? (report?.up_to_date ? "Already up to date." : undefined)}
            >
              {running ? "Updating…" : "Update"}
            </Button>
          </div>
        </div>

        {report && (
          <div className="mt-3 text-body">
            {report.up_to_date ? (
              <div className="text-ink-300">
                Up to date — the newest attested green main build is installed.
              </div>
            ) : (
              <>
                <div className="font-semibold text-ink-100">
                  Update available · {report.change_count} change
                  {report.change_count === 1 ? "" : "s"}
                </div>
                <div className="mt-1 text-ink-300">
                  {report.current ?? "nothing"} → <span className="num">{report.target}</span>
                  {report.schema.migration
                    ? ` · schema migration ${report.schema.current} → ${report.schema.target} (covered by the pre-update backup receipt)`
                    : " · no schema change"}
                </div>
                {report.changes.length > 0 && (
                  <ul className="mt-2 list-disc pl-5 text-ink-300">
                    {report.changes.map((c) => (
                      <li key={c}>{c}</li>
                    ))}
                  </ul>
                )}
                {report.blockers.length > 0 && (
                  <ul className="mt-2 text-amber-400">
                    {report.blockers.map((b) => (
                      <li key={b}>blocked: {b}</li>
                    ))}
                  </ul>
                )}
              </>
            )}
            {status?.checked_at ? (
              <div className="mt-2 text-ink-500">
                checked{" "}
                <span className="num">
                  {fmtTime(new Date(status.checked_at * 1000).toISOString())}
                </span>
              </div>
            ) : null}
          </div>
        )}

        {status?.error && <div className="mt-3 text-red-400">{status.error}</div>}
        {error && <div className="mt-3 text-red-400">{error}</div>}

        {(running || (status?.lines.length ?? 0) > 0) && (
          <pre
            ref={linesRef}
            className="mt-3 max-h-64 overflow-auto rounded bg-ink-900/60 p-3 text-body text-ink-200"
          >
            {(status?.lines ?? []).join("\n")}
          </pre>
        )}

        {status?.result && !running && (
          <div className="mt-3 text-body text-ink-300">
            {status.result.rolled_back === true
              ? "The update rolled back to the previous release — see the log above."
              : "Update finished."}
          </div>
        )}
      </div>

      {blocked && <div className="text-body text-ink-500">{blocked}</div>}
    </section>
  );
}
