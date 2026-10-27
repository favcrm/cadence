/** CAD-1604: a failed thread read must NAME what refused.
 *
 *  A board refusal arrives as `{ error: { code, message } }`. The client used
 *  to hand that object straight to `Error.message`, so every surface that
 *  printed the message printed "[object Object]" — the member saw "The thread
 *  could not be read — [object Object]", and neither the member nor the
 *  operator could tell what failed. The message is now always text, taken
 *  from the server's own words.
 */
import { api, ApiError } from "../src/lib/api";
import { errorMessage } from "../src/lib/apiError";
import { Resource } from "../src/lib/cacheCore";
import { openSession } from "../src/features/auth/session";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function stubFetch(status: number, body: unknown): { url: string; init: RequestInit }[] {
  const calls: { url: string; init: RequestInit }[] = [];
  (globalThis as { fetch: unknown }).fetch = async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return {
      ok: status >= 200 && status < 300,
      status,
      statusText: "",
      json: async () => body,
    } as Response;
  };
  return calls;
}

const REFUSAL = "platform assistant provision refused: no enrolled hosted-board service credential";

async function main() {
  // The member surface's own read: the platform answers with the refusal
  // envelope, and the client must carry the refusal's words as text.
  {
    const calls = stubFetch(403, {
      error: { code: "forbidden", message: REFUSAL },
      ok: false,
    });
    let err: ApiError | null = null;
    await api.memberAssistHistory({ limit: 200 }).catch((e: ApiError) => {
      err = e;
    });
    const refusal = err as ApiError | null;
    equal(refusal?.status, 403, "the refusal status crosses");
    equal(refusal?.message, REFUSAL, "the server's own words are the message");
    equal(String(refusal?.message).includes("[object"), false, "the message is never an object");
    equal(calls.length, 1, "one read, one refusal");
    equal(calls[0].url, "/api/member-assistant/history", "the member read path");
  }

  // The banner prints the resource's error text. A failed read must keep
  // the reason — never "[object Object]", never an empty line.
  {
    const resource = new Resource(async () => {
      throw new ApiError(REFUSAL, 403);
    });
    await resource.refresh();
    equal(resource.get().status, "failed", "a refused read is failed, not empty");
    equal(resource.get().error, REFUSAL, "the banner names the refusal");
    equal(String(resource.get().error).includes("[object"), false, "no object leaks into the banner");
  }

  // A refusal whose detail is itself an object still reads as text: the
  // nested message is reached, and an unusable detail falls back to words.
  {
    const resource = new Resource(async () => {
      throw { message: { message: REFUSAL } };
    });
    await resource.refresh();
    equal(resource.get().error, REFUSAL, "a nested message is reached");
    const empty = new Resource(async () => {
      throw { message: { code: "forbidden" } };
    });
    await empty.refresh();
    equal(empty.get().error, "request failed", "an unusable detail falls back to words");
    equal(String(empty.get().error).includes("[object"), false, "still never an object");
  }

  // The helper's own shapes: a bare string error, an object error, and a
  // body that carries no error at all.
  {
    equal(errorMessage({ error: "plain refusal" }, "fallback"), "plain refusal", "a bare string error");
    equal(errorMessage({ error: { message: REFUSAL } }, "fallback"), REFUSAL, "an object error");
    equal(errorMessage({}, "fallback"), "fallback", "no error at all");
    equal(errorMessage(null, "fallback"), "fallback", "a null body");
    equal(errorMessage({ error: "" }, "fallback"), "fallback", "an empty error");
  }

  // The sign-in refusal answers the same envelope shape, and the login
  // screen must name the refusal too — not "[object Object]".
  {
    stubFetch(403, { error: { code: "forbidden", message: "this sign-in link is no longer valid" } });
    let err: ApiError | null = null;
    await openSession("nonce-1").catch((e: ApiError) => {
      err = e;
    });
    equal((err as ApiError | null)?.message, "this sign-in link is no longer valid", "the sign-in refusal names itself");
    equal(String((err as ApiError | null)?.message).includes("[object"), false, "the sign-in refusal is never an object");
  }

  console.log("thread read failure message checks passed");
}

main().catch((error) => {
  console.error(error);
  throw error;
});
