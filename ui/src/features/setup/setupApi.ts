import { errorMessage } from "../../lib/apiError";
import { cache } from "../../lib/resources";
import { privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import type { SetupReport } from "./checks";

/** The error a refused `/api/setup` (read-only board, tailnet viewer) reads as. */
export const SETUP_ON_HOST = "setup runs on the host";
/** A hosted board answers `501 not_implemented`: setup is the platform's job there. */
export const SETUP_HOSTED = "setup is handled for this workspace";

async function fetchSetup(fresh: boolean): Promise<SetupReport> {
  return withPrivateOperation(async (op) => {
    const resp = await privateFetch(fresh ? "/api/setup?fresh=1" : "/api/setup", {
      headers: sessionHeaders(),
    }, op);
    // A feature 403 is host-only setup, not a session loss.
    if (resp.status === 403) {
      op.assertCurrent();
      throw new Error(SETUP_ON_HOST);
    }
    if (!resp.ok) {
      const body = await resp.json().catch(() => null);
      op.assertCurrent();
      const code = body?.error?.code;
      if (resp.status === 501 && code === "not_implemented") throw new Error(SETUP_HOSTED);
      throw new Error(errorMessage(body, `${resp.status} ${resp.statusText}`));
    }
    const report = await resp.json() as SetupReport;
    op.assertCurrent();
    return report;
  });
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
    // Once refused, no later availability error may reveal the prior host report.
    discardOnError: (error) =>
      error instanceof Error &&
      (error.message === SETUP_ON_HOST || error.message === SETUP_HOSTED),
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
