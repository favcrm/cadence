import { ConditionalGet } from "../src/lib/conditionalGet";
function equal(a: unknown, b: unknown, what: string) { if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(`${what}: ${JSON.stringify(a)}`); }
async function main() {
  let session: string | null = "first";
  let response = new Response('{"rows":[1]}', { headers: { ETag: '"one"' } });
  const headers: Record<string, string>[] = [];
  const request = (async (_url: unknown, init: RequestInit) => { headers.push(init.headers as Record<string, string>); return response; }) as typeof fetch;
  const client = new ConditionalGet(request, () => session);
  equal((await client.get("/api/issues?project=a", {})).value, { rows: [1] }, "first response");
  response = new Response(null, { status: 304 });
  equal((await client.get("/api/issues?project=a", {})).value, { rows: [1] }, "304 retains data");
  equal(headers.at(-1), { "If-None-Match": '"one"' }, "same representation validator sent");
  await client.get("/api/issues?project=b", {});
  equal(headers.at(-1), {}, "query variants do not share validator");
  session = null;
  const signedOut = await client.get("/api/issues?project=a", {});
  equal(headers.at(-1), {}, "signout clears validators");
  equal(signedOut.value, undefined, "304 cannot expose old session payload");
  response = new Response('{"rows":[2]}', { headers: { ETag: '"two"' } });
  await client.get("/api/issues", {});
  response = new Response('{"error":"denied"}', { status: 403 });
  await client.get("/api/issues", {});
  response = new Response(null, { status: 304 });
  equal((await client.get("/api/issues", {})).value, undefined, "refusal removes old data");
  let resolve!: (response: Response) => void;
  const delayed = new ConditionalGet((() => new Promise<Response>((r) => { resolve = r; })) as typeof fetch, () => session);
  const pending = delayed.get("/api/outbox", {});
  session = "new";
  resolve(new Response('{"secret":true}', { headers: { ETag: '"secret"' } }));
  await pending;
  const next = delayed.get("/api/outbox", {});
  resolve(new Response(null, { status: 304 }));
  equal((await next).value, undefined, "old credential request cannot populate new cache");
  console.log("conditional GET checks passed");
}
main().catch((error) => { setTimeout(() => { throw error; }); });
