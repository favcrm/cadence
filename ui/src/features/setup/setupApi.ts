import { cache } from "../../lib/resources";
import type { SetupReport } from "./checks";

/** The error a refused `/api/setup` (read-only board, tailnet viewer) reads as. */
export const SETUP_ON_HOST = "setup runs on the host";

async function fetchSetup(fresh: boolean): Promise<SetupReport> {
  const resp = await fetch(fresh ? "/api/setup?fresh=1" : "/api/setup");
  if (resp.status === 403) throw new Error(SETUP_ON_HOST);
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
export const setupResource = cache.resource<SetupReport>(
  "setup",
  () => fetchSetup(false),
  {
    freshMs: 60_000,
  },
);

/**
 * Re-check's explicit fresh=1 request uses the shared observation lifecycle:
 * errors stay stale (including host-only refusals) after navigation, and a
 * later visit retries them. No fresh flag can leak into another fetch.
 */
export async function recheckSetup(): Promise<SetupReport> {
  await setupResource.refreshUsing(() => fetchSetup(true));
  const state = setupResource.get();
  if (state.error) throw new Error(state.error);
  if (!state.data) throw new Error("Setup report unavailable");
  return state.data;
}
