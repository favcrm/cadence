/** CAD-1611 / CAD-1840: no refusal surface prints "[object Object]".
 *  The bodies below are the board/Worker's real refusal envelope. */
import { setupResource, SETUP_HOSTED } from "../src/features/setup/setupApi";
import { csvActions } from "../src/features/app-shell/csvClient";
import { sendClient } from "../src/features/app-shell/sendClient";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function stubFetch(status: number, body: unknown): void {
  (globalThis as { fetch: unknown }).fetch = async () =>
    ({ ok: status >= 200 && status < 300, status, statusText: "", json: async () => body }) as Response;
}

async function main() {
  // The tenant-board Worker's live answer to GET /api/setup (CAD-1840).
  stubFetch(501, {
    ok: false,
    error: { code: "not_implemented", message: "This endpoint is not available on the isolated board." },
  });
  await setupResource.refresh();
  const setup = setupResource.get();
  equal(setup.error, SETUP_HOSTED, "a hosted board's 501 maps to the hosted sentinel");
  equal(String(setup.error).includes("[object"), false, "the setup error is never an object");

  // Any other setup refusal reads as its words, not an object.
  stubFetch(500, { error: { code: "boom", message: "probe crashed" } });
  await setupResource.refresh();
  equal(setupResource.get().error, "probe crashed", "an object refusal reads as its message");

  // App-shell clients: the object envelope reaches the surface's error text.
  stubFetch(409, { error: { code: "stale", message: "the list changed" } });
  const csv = await csvActions.preview({ installId: "i", contextId: "c" }, "display_name\nA\n").catch((e: Error) => e);
  equal((csv as Error).message, "the list changed", "csv refusal names itself");
  const send = await sendClient.smtpShow({ installId: "i", contextId: "c" }).catch((e: Error & { status?: number }) => e);
  equal((send as Error).message, "the list changed", "send refusal names itself");
  equal((send as { status?: number }).status, 409, "the status still crosses");

  console.log("error envelope surface checks passed");
}

main().catch((error) => {
  console.error(error);
  throw error;
});
