import { ApiError } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation } from "../../lib/privateSession";

/** One canvas artifact version's bytes through the board (operator
 *  session). The conversation in view is a required query field — the
 *  daemon refuses an artifact of any other conversation or workspace.
 *  Kept out of `lib/api.ts`, which the app-fixture drift baseline pins. */
export function artifactBytes(
  id: string,
  version: number,
  conversation: string,
  signal?: AbortSignal,
): Promise<{ bytes: ArrayBuffer; mime: string }> {
  return usingPrivateOperation(undefined, async (op) => {
    const url = `/api/artifacts/${encodeURIComponent(id)}/${version}?conversation=${encodeURIComponent(conversation)}`;
    const resp = await privateFetch(url, { headers: sessionHeaders(), signal }, op);
    if (!resp.ok) {
      const body = await resp.json().catch(() => null);
      throw new ApiError(typeof body?.error === "string" ? body.error : `${resp.status} ${resp.statusText}`, resp.status);
    }
    const bytes = await resp.arrayBuffer();
    op.assertCurrent();
    return { bytes, mime: resp.headers.get("Content-Type") ?? "application/octet-stream" };
  });
}
