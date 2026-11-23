import {
  canvasSnapshot,
  closeTab,
  feedCanvas,
  focusTab,
  openAppView,
  openArtifact,
  patchAppViewRoute,
  resetCanvas,
} from "../src/features/canvas/canvasStore";
import { appViewId, appViewRef, applyRoutePatch, sameRoute } from "../src/features/canvas/appView";
import { appSectionHref } from "../src/features/app-shell/appNav";
import { stripAppRouteParams } from "../src/lib/router";
import { setQueryParams } from "../src/features/workspace-apps/screen/routeParams";
import { resourceAppView } from "../src/features/app-shell/assistantResourceNavigation";
import { frameRoute, parseRoutePatch, routeFromHref } from "../src/features/workspace-apps/screen/screenRoute";
import { readOnlyReceipt, READ_ONLY_OPERATIONS } from "../src/features/workspace-apps/screen/screenReadOnly";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import { SCREEN_READ_VERBS } from "../src/features/workspace-apps/screen/screenServices";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const ref = (record: string, over: Record<string, unknown> = {}) => ({
  installId: "inst_crm",
  tag: "crm",
  contextId: "ctx_1",
  route: { section: "customers", view: "list", record_id: record, segment_id: null },
  title: `Customer ${record}`,
  href: `/apps/crm?install=inst_crm&ctx=ctx_1&crm=customers&record=${record}`,
  ...over,
});

// A malformed reference opens no tab and changes nothing.
resetCanvas("c1", "k", "app");
const hostile = "x".repeat(200);
for (const bad of [
  null, "crm", [], {},
  ref("cus_1", { installId: "../etc" }),
  ref("cus_1", { tag: "Bad Tag" }),
  ref("cus_1", { contextId: "a b" }),
  ref("cus_1", { title: "  " }),
  ref("cus_1", { title: `${hostile}y` }),
  ref("cus_1", { href: "https://evil.example/apps/crm" }),
  ref("cus_1", { href: "//evil.example/apps/crm" }),
  ref("cus_1", { href: "/apps/../settings" }),
  ref("cus_1", { route: { section: "customers", view: "edit", record_id: null, segment_id: null } }),
  ref("cus_1", { route: { section: "Customers", view: "list", record_id: null, segment_id: null } }),
  ref("cus_1", { route: { section: "customers", view: "list", record_id: "CUS 1", segment_id: null } }),
]) {
  equal(openAppView(bad), false, `refused ${JSON.stringify(bad)?.slice(0, 60)}`);
}
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "no tab, drawer stays closed");

// Open, focus, close: a tap adds the tab and opens the drawer; the same record focuses it.
equal(openAppView(ref("cus_1")), true, "valid ref opens");
const first = appViewId(appViewRef(ref("cus_1"))!);
equal([canvasSnapshot().tabs.map((t) => t.id), canvasSnapshot().active, canvasSnapshot().open], [[first], first, true], "tab opened and active");
equal(canvasSnapshot().tabs[0].title, "Customer cus_1", "tab titled by the record");
openAppView(ref("cus_1"));
equal(canvasSnapshot().tabs.length, 1, "same record focuses, no duplicate");
openAppView(ref("cus_2"));
const second = appViewId(appViewRef(ref("cus_2"))!);
equal([canvasSnapshot().tabs.length, canvasSnapshot().active], [2, second], "another record is another tab");
openArtifact({ id: "art-a", version: 1, kind: "html", title: "doc", bytes: 1 });
focusTab(first);
equal(canvasSnapshot().active, first, "focus selects the tab");
closeTab(first);
equal(canvasSnapshot().active, second, "closing the active tab selects a neighbour");
closeTab(second);
closeTab("art-a");
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "closing the last tab collapses the drawer");
openAppView(ref("cus_1"));
equal(canvasSnapshot().open, true, "reopens from the same reference");

// The feed never opens an app view, never drops one, and a conversation change closes it.
feedCanvas({ conversation: "c1", surface: "app", composerKey: "k", cards: [], working: null, turn: "m1" });
equal(canvasSnapshot().tabs.map((t) => t.id), [first], "a feed pass keeps the tab");
feedCanvas({ conversation: "c2", surface: "app", composerKey: "k", cards: [], working: null, turn: null });
equal([canvasSnapshot().tabs.length, canvasSnapshot().open], [0, false], "conversation change closes the tab");

// The route stays on the tab: a patch moves it there, and a frame that is
// handed one never reads the board URL.
resetCanvas("c1", "k", "app");
openAppView(ref("cus_1"));
patchAppViewRoute(first, { record_id: "cus_9", segment_id: null });
equal(canvasSnapshot().tabs[0].app?.route, { section: "customers", view: "list", record_id: "cus_9", segment_id: null }, "patch applied to the tab route");
equal(canvasSnapshot().tabs[0].id, first, "the tab keeps its identity");
const nav = { section_query: "crm", default_section: "customers", sections: [{ id: "customers", label: "Customers" }, { id: "segments", label: "Segments" }] };
const board = "/apps/crm?crm=segments&record=cus_other";
equal(frameRoute(nav, "crm", { href: board, route: { section: "customers", view: "list", record_id: "cus_1", segment_id: null } }), { section: "customers", view: "list", record_id: "cus_1", segment_id: null }, "a held route ignores the board URL");
equal(frameRoute(nav, "crm", { href: board }).record_id, "cus_other", "no held route: the board URL decides");
equal(frameRoute(nav, "crm", { href: board, route: { section: "unknown", view: "list", record_id: "cus_1", segment_id: null } }).section, "customers", "an unknown section falls to the app's default");

// The trigger: a CRM customer resource becomes a reference; anything else, or
// no screen tag, leaves only the plain link.
const customer = { kind: "customer", id: "cus_8f21", label: "Harbour Trading" };
equal(resourceAppView("/apps/crm", "inst_crm", "ctx_1", "crm", customer)?.route, { section: "customers", view: "list", record_id: "cus_8f21", segment_id: null }, "customer route");
equal(resourceAppView("/apps/crm", "inst_crm", "ctx_1", null, customer), null, "no screen tag: no canvas action");
equal(resourceAppView(null, "inst_crm", "ctx_1", "crm", customer), null, "no verified app path: no canvas action");
equal(resourceAppView("/apps/crm", "inst_crm", "ctx_1", "crm", { kind: "invoice", id: "x", label: "x" }), null, "unknown kind: no canvas action");
equal(resourceAppView("/apps/crm", "inst_crm", "ctx_1", "crm", { kind: "customer", id: "Cus_UPPER", label: "x" }), null, "an id the frame would refuse: no canvas action");

// Read-only: the mount keeps only read operations; a write the app declared is closed.
const receipt = { mount: "/api/app-screen/n", bridge_nonce: "b", generation: 1, tag: "crm", action_token: "t", tools: [], host_contract: "screen-records.v1", operations: ["records.list", "records.show", "records.update", "records.create", "sends.approve", "screen.route.patch", "records.csv.import", "works.get"] } as unknown as MountReceipt;
equal(readOnlyReceipt(receipt).operations, ["records.list", "records.show", "screen.route.patch"], "only reads and route patches survive");
const aggregateReceipt = { ...receipt, operations: ["records.aggregate", "records.batch", "records.update", "records.list"] } as unknown as MountReceipt;
equal(readOnlyReceipt(aggregateReceipt).operations, ["records.aggregate", "records.list"], "records.aggregate is kept, writes still dropped");
equal([...READ_ONLY_OPERATIONS].sort(), [...SCREEN_READ_VERBS].sort(), "canvas read-only set matches the screen read set");
equal(receipt.operations?.length, 8, "the original receipt is not mutated");

// CAD-2555: a route's params. A route without them has its old shape; the board URL carries them as `p.<name>`.
const paramsRoute = { section: "reports", view: "list", record_id: "pnl", segment_id: null, params: { from: "2026-07-01", to: "2026-09-30" } };
equal(appViewRef(ref("pnl", { route: paramsRoute }))?.route, paramsRoute, "a route with params is a reference");
equal(appViewRef(ref("pnl", { route: { ...paramsRoute, params: {} } }))?.route.params, undefined, "empty params are no params");
for (const params of [[], "x", { From: "a" }, { from: "" }, { from: "x".repeat(201) }, { from: "a\u0007" }, { from: 1 }, Object.fromEntries("abcdefg".split("").map((k) => [k, "x"]))]) {
  equal(appViewRef(ref("pnl", { route: { ...paramsRoute, params } })), null, `refused params ${JSON.stringify(params)?.slice(0, 40)}`);
}
const reportsNav = { section_query: "crm", default_section: "customers", sections: [{ id: "customers", label: "Customers" }, { id: "reports", label: "Reports" }] };
const paramsHref = "/apps/crm?crm=reports&record=pnl&p.from=2026-07-01&p.to=2026-09-30";
equal(routeFromHref(reportsNav, "crm", paramsHref), paramsRoute, "p.* in the board URL is the route's params");
equal(routeFromHref(reportsNav, "crm", "/apps/crm?crm=reports").params, undefined, "no p.* is no params key");
equal(routeFromHref(reportsNav, "crm", "/apps/crm?crm=reports&p.from=ok&p.To=bad").params, undefined, "a bad set is dropped whole");
equal(parseRoutePatch({ section: "reports", params: { from: "2026-07-01" } }, reportsNav), { section: "reports", params: { from: "2026-07-01" } }, "a patch may carry params");
equal(parseRoutePatch({ params: { From: "2026-07-01" } }, reportsNav), null, "a patch with a bad param name is refused");
equal(parseRoutePatch({ params: ["x"] }, reportsNav), null, "a patch with non-object params is refused");
equal(parseRoutePatch({ params: { a: 1 } }, reportsNav), null, "a patch with a non-string param is refused");
// The round trip: URL -> route -> patch -> URL.
const q = new URLSearchParams("crm=reports&record=pnl&p.from=2026-07-01&p.to=2026-09-30&keep=1");
setQueryParams(q, { from: "2026-10-01" });
equal(q.toString(), "crm=reports&record=pnl&keep=1&p.from=2026-10-01", "a patch replaces the page's p.* and leaves other keys");
setQueryParams(q, {});
equal(q.toString(), "crm=reports&record=pnl&keep=1", "empty params clear p.*");
equal(appSectionHref("/apps/crm?crm=reports&p.from=2026-07-01&p.to=2026-09-30", reportsNav, "customers"), "/apps/crm", "section navigation drops the old page's params");
equal(new URLSearchParams(stripAppRouteParams("p.from=2026-07-01&record=pnl&project=p1")).toString(), "project=p1", "leaving the app drops p.* but not host keys");
equal(sameRoute(paramsRoute as never, { ...paramsRoute, params: { from: "2026-07-01", to: "2026-09-30" } } as never), true, "same params are the same route");
equal(sameRoute(paramsRoute as never, { ...paramsRoute, params: { from: "2026-07-01", to: "2026-09-29" } } as never), false, "another value is another route");
equal(sameRoute(paramsRoute as never, { ...paramsRoute, params: undefined } as never), false, "params against none is another route");
equal(applyRoutePatch(paramsRoute as never, { record_id: "x" }).params, paramsRoute.params, "a patch without params keeps them");
