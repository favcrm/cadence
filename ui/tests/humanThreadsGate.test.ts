/**
 * CAD-1704: human rooms live only on the hosted public surface. The board's
 * notification inbox must mount there and nowhere else, so a loopback or
 * tailnet board never polls `/api/human-threads/notifications` (which it
 * does not serve) every 15 seconds.
 */
import { humanThreadsAvailable } from "../src/lib/humanThreads";

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

const session = { id: "s", origin: "loopback" as const, created: 0, last_used: 0, idle_expires_at: 0, expires_at: 0, user_agent: "t", user: null, account_id: null };

equal(humanThreadsAvailable({ hosted: true }), true, "a hosted board serves human threads");
equal(
  humanThreadsAvailable({ hosted: false, session: { ...session, origin: "public" } }),
  true,
  "a public-origin session serves human threads",
);
equal(
  humanThreadsAvailable({ hosted: false, session }),
  false,
  "a loopback board has no human-threads route",
);
equal(
  humanThreadsAvailable({ session: { ...session, origin: "tailnet" } }),
  false,
  "a tailnet board has no human-threads route",
);
equal(humanThreadsAvailable(null), false, "an absent meta mounts nothing");
equal(humanThreadsAvailable({}), false, "a board that reports neither hosted nor a session mounts nothing");

console.log("human threads surface checks passed");
