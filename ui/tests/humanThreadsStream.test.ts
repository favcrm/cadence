import { allowPrivateReads, resetPrivateSession } from "../src/lib/privateSession";
import type { SseOptions, SseSubscription } from "../src/lib/sse";
import {
  createHumanThreadsStreamStore,
  parseHumanThreadsFrame,
  type HumanThreadsPushMessage,
} from "../src/features/threads/humanThreadsStream";

function assert(cond: unknown, why: string): asserts cond {
  if (!cond) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

function main() {
  allowPrivateReads();

  equal(
    parseHumanThreadsFrame('{"scopes":["rooms","room"],"room":"grp_1"}'),
    { scopes: ["rooms", "room"], room: "grp_1" },
    "parses valid scopes + room frame",
  );
  equal(
    parseHumanThreadsFrame('{"scopes":["notifications"]}'),
    { scopes: ["notifications"] },
    "parses room-less notification scope",
  );
  equal(parseHumanThreadsFrame("not-json"), null, "ignores invalid JSON");
  equal(parseHumanThreadsFrame('{"scopes":[]}'), null, "ignores empty scopes");
  equal(parseHumanThreadsFrame('{"scopes":["unknown"]}'), null, "ignores unknown scopes");

  let openCalls = 0;
  let closeCalls = 0;
  let currentOpts: SseOptions | null = null;
  const store = createHumanThreadsStreamStore((opts) => {
    openCalls += 1;
    currentOpts = opts;
    const sub: SseSubscription = {
      close: () => {
        closeCalls += 1;
      },
      lastEventId: () => null,
    };
    return sub;
  });

  const msgsA: HumanThreadsPushMessage[] = [];
  const msgsB: HumanThreadsPushMessage[] = [];
  const unsubA = store.subscribe((m) => msgsA.push(m));
  const unsubB = store.subscribe((m) => msgsB.push(m));
  assert(openCalls === 1, "ref-counts shared subscribers onto a single SSE stream");

  currentOpts!.onOpen?.();
  assert(store.isLive(), "marks store live after onOpen");
  equal(msgsA[0], { kind: "open", reconnected: false }, "first open reports reconnected=false");
  equal(msgsB[0], { kind: "open", reconnected: false }, "sibling receives open notification");

  currentOpts!.onEvent({ type: "human_threads", data: '{"scopes":["room","rooms"],"room":"grp_9"}', id: null });
  currentOpts!.onEvent({ type: "human_threads", data: "{bad", id: null });
  equal(
    msgsA[1],
    { kind: "event", scopes: ["room", "rooms"], room: "grp_9" },
    "delivers parsed push event",
  );
  assert(msgsA.length === 2, "ignores malformed frame");

  // Transient interruption followed by reconnect triggers catch-up open
  currentOpts!.onError?.("reconnecting");
  assert(!store.isLive(), "marks degraded while reconnecting");
  equal(msgsA[2], { kind: "degraded" }, "emits degraded state");
  currentOpts!.onOpen?.();
  equal(msgsA[3], { kind: "open", reconnected: true }, "reconnect emits reconnected=true");

  // Unsubscribing one keeps stream open; unsubscribing all closes it
  unsubA();
  equal(closeCalls, 0, "keeps stream alive while one subscriber remains");
  unsubB();
  equal(closeCalls, 1, "closes stream when last subscriber unsubscribes");

  // Permanent 404 stops stream for session and does not retry until reset()
  const msgsC: HumanThreadsPushMessage[] = [];
  const unsubC = store.subscribe((m) => msgsC.push(m));
  equal(openCalls, 2, "reopens cleanly when new subscriber mounts");
  currentOpts!.onError?.("stopped");
  assert(!store.isLive(), "stopped stream is not live");
  equal(closeCalls, 2, "closes stopped stream");
  unsubC();

  const msgsD: HumanThreadsPushMessage[] = [];
  const unsubD = store.subscribe((m) => msgsD.push(m));
  equal(openCalls, 2, "does not retry permanently stopped stream within same session");
  unsubD();

  store.reset();
  const unsubE = store.subscribe(() => {});
  equal(openCalls, 3, "reset allows reopening on new session");
  unsubE();

  resetPrivateSession();
  console.log("humanThreadsStream checks passed");
}

main();
