export {};
/**
 * AOS-816 (CAD-2885): unit tests for member project/wiki access requests,
 * owner scope narrowing rules, To-do row parsing, and Home thread card items.
 */

declare function require(name: string): any;
declare const process: { exit(code?: number): never };
const assert = require("assert");

const {
  accessTargetHref,
  allowedGrantScopes,
  canNarrowAccessScope,
  confirmAccessOffer,
  parseAccessOfferEntry,
  parseAccessRequestEntry,
} = require("../src/features/home/accessRequest") as typeof import("../src/features/home/accessRequest");
const { parseStagedList, titleOf } = require("../src/features/decisions/stagedWrite") as typeof import("../src/features/decisions/stagedWrite");
const { threadItems } = require("../src/features/home/thread") as typeof import("../src/features/home/thread");

const arId = `ar_${"a".repeat(32)}`;
const aroId = `aro_${"b".repeat(32)}`;
const convId = "tc_11111111-2222-4333-8444-555555555555";
const iso = "2026-11-27T17:00:00.000Z";

// 1. Scope narrowing only (never widening)
assert.deepEqual(allowedGrantScopes("project", "project_member"), ["project_member", "wiki_read"]);
assert.deepEqual(allowedGrantScopes("project", "wiki_read"), ["wiki_read"]);
assert.deepEqual(allowedGrantScopes("wiki", "wiki_write"), ["wiki_write", "wiki_read"]);
assert.deepEqual(allowedGrantScopes("wiki", "wiki_read"), ["wiki_read"]);
assert.equal(canNarrowAccessScope("project", "project_member", "wiki_read"), true);
assert.equal(canNarrowAccessScope("wiki", "wiki_write", "wiki_read"), true);
assert.equal(canNarrowAccessScope("wiki", "wiki_read", "wiki_write"), false);
assert.equal(canNarrowAccessScope("wiki", "wiki_read", "project_member"), false);

// 2. Destination hrefs for unlocked targets
assert.equal(accessTargetHref("project", "q4-pricing", "project_member"), "/projects/q4-pricing");
assert.equal(accessTargetHref("project", "q4-pricing", "wiki_read"), "/wiki/projects/q4-pricing");
assert.equal(accessTargetHref("wiki", "global/brands/style.md", "wiki_write"), "/wiki/global/brands/style.md");

// 3. Offer & card parsing in Home thread
const offer = parseAccessOfferEntry({
  role: "agent",
  kind: "access_request",
  payload: {
    v: 1,
    id: aroId,
    status: "offered",
    conversation: convId,
    kind: "project",
    target: "q4-pricing",
    target_label: "Project · q4-pricing",
    requested_scope: "project_member",
    reason: "Preparing Q4 renewal sheet",
    expires_at: iso,
  },
});
assert(offer !== null, "valid offer parses");
assert.equal(offer?.target, "q4-pricing");

const approvedCard = parseAccessRequestEntry({
  role: "agent",
  kind: "access_request",
  payload: {
    v: 1,
    id: arId,
    status: "approved",
    kind: "project",
    target: "q4-pricing",
    target_label: "Project · q4-pricing",
    requested_scope: "project_member",
    granted_scope: "wiki_read",
    duration: "7d",
    requested_at: iso,
    decided_at: iso,
    decided_by: "Chris Chiang",
  },
});
assert(approvedCard !== null, "valid approved card parses");
assert.equal(approvedCard?.grantedScope, "wiki_read");
assert.equal(approvedCard?.duration, "7d");

// Extra unknown property fails strict check
assert.equal(
  parseAccessRequestEntry({
    role: "agent",
    kind: "access_request",
    payload: {
      v: 1,
      id: arId,
      status: "pending",
      kind: "wiki",
      target: "global/brands",
      target_label: "Wiki · global/brands",
      requested_scope: "wiki_write",
      requested_at: iso,
      unexpected: true,
    },
  }),
  null,
);

// 4. Thread items keep access_request as "access" card without splitting tool groups
const items = threadItems({
  entries: [
    {
      seq: 1,
      role: "agent",
      kind: "access_request",
      text: "Pending access request",
      payload: {
        v: 1,
        id: arId,
        status: "pending",
        kind: "wiki",
        target: "global/brands",
        target_label: "Wiki · global/brands",
        requested_scope: "wiki_write",
        requested_at: iso,
      },
    },
  ],
  pending: [],
} as any);
assert.equal(items.length, 1);
assert.equal(items[0]?.type, "access");

// 5. Staged actions list parses access_request for owner Home To-do rail
const staged = parseStagedList({
  actions: [
    {
      id: "act_1",
      tool: "access_request",
      status: "pending",
      createdAt: 1_795_800_000_000,
      input: {
        request_id: arId,
        requester: "Patrick Wun",
        requester_handle: "@patrick-wun",
        kind: "project",
        target: "q4-pricing",
        target_label: "Project · q4-pricing",
        requested_scope: "project_member",
        reason: "Need pricing matrix",
        requested_at: iso,
      },
    },
  ],
});
assert(staged !== null && staged.length === 1, "staged access_request parses cleanly");
assert.equal(staged![0]!.kind, "access_request");
assert.equal(titleOf(staged![0]!, (s) => s), "Patrick Wun asks for Project · q4-pricing");

// 6. Confirm sends edited reason JSON body
(async () => {
  const seen: Array<{ url: string; body: string | undefined }> = [];
  const origFetch = globalThis.fetch;
  (globalThis as any).fetch = async (url: unknown, init?: RequestInit) => {
    seen.push({ url: String(url), body: init?.body as string | undefined });
    return new Response(JSON.stringify({ ok: true, outcome: "sent" }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };
  try {
    const outcome = await confirmAccessOffer(offer!, "Need Q4 figures");
    assert.equal(outcome, "sent");
    assert.equal(seen.length, 1);
    assert(seen[0]!.url.includes(`/api/access-requests/offers/${aroId}/confirm?conversation=`));
    assert.equal(seen[0]!.body, JSON.stringify({ reason: "Need Q4 figures" }));
  } finally {
    (globalThis as any).fetch = origFetch;
  }
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
