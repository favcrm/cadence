export {};
declare function require(name: string): any;

import type { UnifiedTodoItem } from "../src/features/home/railModel";

const {
  countFeedCategories,
  feedCategoriesForAgent,
  feedCategoriesForDecidedWrite,
  feedCategoriesForInfoNeed,
  presentTodoCategories,
  sortUnifiedTodos,
  todoCategoryForNeed,
  todoPresentation,
} = require("../src/features/home/railModel") as typeof import("../src/features/home/railModel");
const { homeNeed } = require("../src/features/home/needs") as typeof import("../src/features/home/needs");

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`railModel test failed: ${msg}`);
}

// 1. To do category classification
assert(todoCategoryForNeed({ kind: "staged_write" }) === "approvals", "staged_write is approval");
assert(todoCategoryForNeed({ kind: "master_permission" }) === "approvals", "master_permission is approval");
assert(todoCategoryForNeed({ kind: "merge_decision" }) === "approvals", "merge_decision is approval");
assert(todoCategoryForNeed({ kind: "plan" }) === "reviews", "plan is review");
assert(todoCategoryForNeed({ kind: "idea_plan" }) === "reviews", "idea_plan is review");
assert(todoCategoryForNeed({ kind: "question" }) === "reviews", "question is review");
assert(todoCategoryForNeed({ kind: "blocked" }) === "blocked", "blocked is blocked");
assert(todoCategoryForNeed({ kind: "fenced" }) === "blocked", "fenced is blocked");
assert(todoCategoryForNeed({ kind: "stopped" }) === "blocked", "stopped is blocked");

// 2. Priority sorting: Approvals -> Reviews -> Blocked, stable within category
const blockedNeed = homeNeed({ kind: "blocked", audience: "operator", title: "D-5 blocked", age: 60, project: "demo", command: "x", subject: { kind: "issue", id: "D-5" } });
const planNeed = homeNeed({ kind: "plan", audience: "operator", title: "D-1 plan", age: 10, project: "demo", command: "x", subject: { kind: "issue", id: "D-1" } });
const mergeNeed = homeNeed({ kind: "merge_decision", audience: "operator", title: "D-2 merge", age: 20, project: "demo", command: "x", subject: { kind: "issue", id: "D-2" } });

const items: UnifiedTodoItem[] = [
  { kind: "need", key: "b", category: "blocked", need: blockedNeed },
  { kind: "waiting_routine", key: "r1", category: "reviews", routine: { id: "rt1", name: "Daily", status: "unapproved", approval_drift: false, approved_rev: null, approved_sha256: null, agent_alias: "a1" } as any },
  { kind: "need", key: "p", category: "reviews", need: planNeed },
  { kind: "need", key: "m", category: "approvals", need: mergeNeed },
  { kind: "routine_proposal", key: "rp", category: "approvals", proposal: { id: "p1", title: "Draft", kind: "wiki_propose", proposed_by: "a1" } as any },
];
const sorted = sortUnifiedTodos(items);
assert(
  sorted.map((i) => i.key).join(",") === "m,rp,r1,p,b",
  `sortUnifiedTodos keeps Approvals -> Reviews -> Blocked stably (got ${sorted.map((i) => i.key).join(",")})`,
);

const cats = presentTodoCategories(sorted);
assert(
  cats.map((c) => `${c.key}:${c.count}`).join(",") === "approvals:2,reviews:2,blocked:1",
  `presentTodoCategories counts (got ${cats.map((c) => `${c.key}:${c.count}`).join(",")})`,
);

const pres = todoPresentation(blockedNeed);
assert(pres.tone === "fail" && pres.cueLabel === "Inspect", "blocked presentation uses fail dot and Inspect cue");

// 3. Feed categorization & counting
assert(feedCategoriesForDecidedWrite({ connected: null }).join(",") === "completed", "internal decided write is completed");
assert(feedCategoriesForDecidedWrite({ connected: { app: "gmail", account: "a", effect: "send" } as any }).join(",") === "completed,external", "connected decided write is completed+external");

const workingCats = feedCategoriesForAgent({ running: 1 }, true, [{ label: "Uploaded report.pdf", text: "uploaded" }]);
assert(workingCats.includes("working") && workingCats.includes("uploaded"), "working + upload update");

const idleCats = feedCategoriesForAgent({ running: 0 }, false, []);
assert(idleCats.join(",") === "completed", "idle agent with no notes defaults to completed");

const infoUpload = feedCategoriesForInfoNeed({ kind: "doc", title: "Uploaded spec.pdf" });
assert(infoUpload.join(",") === "uploaded", "info upload classified as uploaded");

const feedTotals = countFeedCategories([
  { categories: workingCats },
  { categories: ["completed", "external"] },
]);
assert(
  feedTotals.all === 2 && feedTotals.working === 1 && feedTotals.uploaded === 1 && feedTotals.completed === 1 && feedTotals.external === 1,
  `countFeedCategories totals mismatch: ${JSON.stringify(feedTotals)}`,
);

console.log("railModel tests passed");
