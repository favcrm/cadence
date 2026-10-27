export {};
/**
 * CAD-1189: a busy daemon (503 / resource_busy) is retried with bounded
 * backoff; any other failure surfaces at once; abort stops the retries.
 */
import { ApiError } from "../src/lib/api";
import { BUSY_BACKOFF_MS, isBusyError, loadFailureMessage, loadWithBackoff } from "../src/features/workspace-apps/installationsLoad";

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
const busy = () => new ApiError("busy", 503, { code: "resource_busy" });
const noWait = async () => undefined;

async function main() {
  let calls = 0;
  const waits: number[] = [];
  const out = await loadWithBackoff(async () => {
    calls += 1;
    if (calls < 3) throw busy();
    return ["app"];
  }, { signal: new AbortController().signal, sleep: async (ms) => { waits.push(ms); } });
  equal(out, ["app"], "the later answer wins");
  equal(waits, [1000, 2000], "backoff steps 1 s then 2 s");

  calls = 0;
  let thrown: unknown = null;
  try {
    await loadWithBackoff(async () => { calls += 1; throw busy(); }, { signal: new AbortController().signal, sleep: noWait });
  } catch (e) { thrown = e; }
  equal(isBusyError(thrown), true, "busy past the last step surfaces the error");
  equal(calls, 1 + BUSY_BACKOFF_MS.length, "one try plus three retries");

  calls = 0;
  thrown = null;
  try {
    await loadWithBackoff(async () => { calls += 1; throw new ApiError("bad", 400); }, { signal: new AbortController().signal, sleep: noWait });
  } catch (e) { thrown = e; }
  equal([calls, thrown instanceof ApiError], [1, true], "a non-busy failure is not retried");

  const c = new AbortController();
  calls = 0;
  thrown = null;
  try {
    await loadWithBackoff(async () => { calls += 1; throw busy(); }, { signal: c.signal, sleep: async () => c.abort() });
  } catch (e) { thrown = e; }
  equal([calls, thrown !== null], [1, true], "abort stops retrying");
  const down = new ApiError("down", 503, { code: "daemon_unavailable" });
  equal(isBusyError(down), false, "daemon_unavailable is not busy");
  equal(isBusyError(new ApiError("x", 503)), false, "a bare 503 is not busy");
  equal(loadFailureMessage(down).startsWith("Daemon unreachable"), true, "daemon_unavailable reads as unreachable");
  calls = 0;
  try {
    await loadWithBackoff(async () => { calls += 1; throw down; }, { signal: new AbortController().signal, sleep: noWait });
  } catch { /* expected */ }
  equal(calls, 1, "daemon_unavailable is not retried");
  console.log("installations load checks passed");
}
void main();
