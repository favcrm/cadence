import { cache } from "../../lib/resources";
import type { SetupReport } from "./checks";

let forceFresh = false;

async function fetchSetup(): Promise<SetupReport> {
  const fresh = forceFresh;
  forceFresh = false;
  const resp = await fetch(fresh ? "/api/setup?fresh=1" : "/api/setup");
  if (!resp.ok) {
    const body = await resp.json().catch(() => null);
    throw new Error(body?.error ?? `${resp.status} ${resp.statusText}`);
  }
  return resp.json() as Promise<SetupReport>;
}

/**
 * `GET /api/setup` — setup's checks, detect only. Slow cold (provider
 * probes, bounded at 5 s each), so fresh for a minute: Home and /setup
 * share one run.
 */
export const setupResource = cache.resource<SetupReport>("setup", fetchSetup, {
  freshMs: 60_000,
});

/** Re-check on demand: the board runs every probe again. */
export function recheckSetup(): Promise<void> {
  forceFresh = true;
  return setupResource.refresh();
}
