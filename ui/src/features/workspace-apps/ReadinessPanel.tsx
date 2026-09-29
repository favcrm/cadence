import { readinessFor } from "./bindingChoices";
import type {
  AppBinding,
  Connection,
  Installation,
} from "./workspaceApps";

/**
 * The installed-App Ready-to-run view (CAD-796): one row per typed
 * capability slot naming its bound connection and destination, current
 * health and custody, whether installation approval is in force, and the
 * plain next action when anything is missing, stale, revoked, unhealthy
 * or unapproved. Purely presentational — every value comes from the
 * installed bundle, saved bindings, listed connections and approval flag
 * the board already fetched, so it can never invent authority.
 */
export function ReadinessPanel({
  installation,
  bindings,
  connections,
  contextId,
}: {
  installation: Installation;
  bindings: AppBinding[];
  connections: Connection[];
  contextId: string | null;
}) {
  const rows = readinessFor(installation, bindings, connections, contextId);
  if (rows.length === 0) return null;
  const ready = rows.every((row) => row.ready);
  return (
    <section className="wa-panel wa-stack" aria-label="Ready to run">
      <h2>Ready to run</h2>
      <p className="wa-kicker" data-tone={ready ? "ok" : "warn"}>
        {ready
          ? "Every connection slot is bound, healthy and approved."
          : "Something still needs you before this app can run."}
      </p>
      {rows.map((row) => (
        <div key={row.slot} className="wa-step" aria-label={`${row.slot} readiness`}>
          <span>
            {titleFor(row.slot)} · {row.requirement}
          </span>
          <span className="wa-status" data-tone={row.ready ? "ok" : "warn"}>
            {row.ready ? "Ready" : "Needs you"}
          </span>
          <p className="wa-muted">
            Connection:{" "}
            {row.connection
              ? `${row.connection.provider}/${row.connection.account} (${row.connection.id})`
              : "none"}
            {row.binding ? ` · revision ${row.binding.revision}` : ""} · health{" "}
            {row.health} · custody {row.custody ? "available" : "unavailable"} ·
            approval {row.approved ? "in force" : "required"}
          </p>
          {!row.ready && <p className="wa-muted">{row.nextAction}</p>}
        </div>
      ))}
    </section>
  );
}

/** "publication" → "Publication" — slot ids in plain words. */
function titleFor(slot: string): string {
  const head = slot.charAt(0).toUpperCase();
  return `${head}${slot.slice(1)}`;
}
