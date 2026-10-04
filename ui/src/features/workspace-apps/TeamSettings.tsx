import { useEffect, useState } from "react";
import Button from "../../ui/Button";
import { workspaceApps, type Agent, type InstallTeam, type Installation } from "./workspaceApps";

/** The input names that must be distinct registered workers: the roles the
 *  installed workflows declare. Generic: read from the bundle, never named here. */
export function teamRoles(installation: Installation): string[] {
  const roles = new Set<string>();
  for (const flow of installation.workflows ?? []) for (const name of flow.distinct ?? []) roles.add(name);
  return [...roles].sort();
}

/**
 * CAD-1123 HP2 — the installation's default team, set once here by the
 * operator so apps never ask for a PM or a worker. One-tap actions in the
 * app's screen start runs with exactly this team; the daemon refuses a run
 * without one. Saving is a compare-and-swap on the stored revision.
 */
export function TeamSettings({ installation, managers, workers, canWrite, busy, mutate }: {
  installation: Installation; managers: Agent[]; workers: Agent[]; canWrite: boolean; busy: boolean;
  mutate: (work: () => Promise<void>) => Promise<void>;
}) {
  const installId = installation.install_id;
  const roles = teamRoles(installation);
  const [team, setTeam] = useState<InstallTeam | null | undefined>(undefined);
  const [owner, setOwner] = useState("");
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    setTeam(undefined);
    workspaceApps.team(installId, controller.signal)
      .then(value => { if (!controller.signal.aborted) { setTeam(value); setOwner(value?.owner_pm ?? ""); setPicked(value?.roles ?? {}); } })
      .catch(() => { if (!controller.signal.aborted) setError("The team could not be read."); });
    return () => controller.abort();
  }, [installId]);
  if (roles.length === 0) return null;
  const ownerGroup = (agent: Agent) => agent.group === owner;
  const candidates = workers.filter(ownerGroup);
  const complete = !!owner && roles.every(role => !!picked[role] && candidates.some(agent => agent.alias === picked[role]));
  const distinct = new Set(roles.map(role => picked[role])).size === roles.length;
  return (
    <section className="wa-panel wa-stack" aria-label="Team">
      <h2>Team</h2>
      <p className="wa-muted">
        {team ? "Actions in this app start with this team." : "Choose who works on this app's runs. Actions stay unavailable until a team is set."}
      </p>
      <label className="wa-field">
        <span>Project manager</span>
        <select value={owner} disabled={!canWrite || busy}
          onChange={event => { setOwner(event.target.value); setPicked({}); }}>
          <option value="">Choose…</option>
          {managers.map(agent => <option key={agent.alias} value={agent.alias}>{agent.alias}</option>)}
        </select>
      </label>
      {roles.map(role => (
        <label key={role} className="wa-field">
          <span>{role.replace(/_/g, " ")}</span>
          <select value={picked[role] ?? ""} disabled={!canWrite || busy || !owner}
            onChange={event => setPicked({ ...picked, [role]: event.target.value })}>
            <option value="">Choose…</option>
            {candidates.map(agent => <option key={agent.alias} value={agent.alias}>{agent.alias}</option>)}
          </select>
        </label>
      ))}
      {!distinct && complete && <p className="wa-alert">Each role needs a different worker.</p>}
      {error && <p className="wa-alert" role="alert">{error}</p>}
      <Button variant="primary" disabled={!canWrite || busy || !complete || !distinct || team === undefined}
        onClick={() => void mutate(async () => {
          try {
            const saved = await workspaceApps.setTeam(installId, { owner_pm: owner, roles: picked, expected_revision: team?.revision ?? 0 });
            setTeam(saved); setError(null);
          } catch { setError("The team was not saved. Reload and try again."); }
        })}>Save team</Button>
    </section>
  );
}
