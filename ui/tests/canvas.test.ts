import {
  drawerLayout,
  canvasSnapshot,
  clampWidth,
  closeDrawer,
  defaultWidth,
  defaultWidthFor,
  closeTab,
  feedCanvas,
  openArtifact,
  resetCanvas,
  selectVersion,
  type FeedCard,
} from "../src/features/canvas/canvasStore";
import { feedParts, plainStep } from "../src/features/canvas/feed";
import { parseCsv } from "../src/features/canvas/csv";
import { askBody, askLabel, makeAskRef } from "../src/features/canvas/askRefs";
import { isArtifactPutCall, planRef, threadItems, toolSteps, type ThreadEntry, type ThreadState } from "../src/features/home/thread";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const card = (id: string, version: number, turn: string, over: Partial<FeedCard> = {}): FeedCard => ({
  key: `${id}@${version}`,
  ref: { id, version, kind: "html", title: id, bytes: 10 },
  live: true,
  operatorTurn: true,
  turn,
  ...over,
});
const feed = (cards: FeedCard[], turn: string | null = "m1", conversation = "c1") =>
  feedCanvas({ conversation, surface: "home", composerKey: "home", cards, working: null, turn });

// The first live artifact of an operator-started turn opens the drawer; a
// later version of it switches the frame without reopening anything.
resetCanvas();
feed([card("art-a", 1, "m1")]);
equal([canvasSnapshot().open, canvasSnapshot().active], [true, "art-a"], "first artifact opens");
feed([card("art-a", 1, "m1"), card("art-a", 2, "m1")]);
equal(canvasSnapshot().tabs[0].selected, 2, "v2 followed");
equal(Object.keys(canvasSnapshot().tabs[0].versions), ["1", "2"], "versions recorded");
selectVersion("art-a", 1);
feed([card("art-a", 1, "m1"), card("art-a", 2, "m1"), card("art-a", 3, "m1")]);
equal([canvasSnapshot().tabs[0].selected, canvasSnapshot().tabs[0].latest], [1, 3], "an older pick is kept");

// Tab rule: history and peer cards never become tabs; a card click adds one.
resetCanvas();
feed([card("art-h1", 1, "m0", { live: false }), card("art-h2", 1, "m0", { live: false }), card("art-h3", 1, "m0", { live: false })]);
equal(canvasSnapshot().tabs.length, 0, "history adds no tabs");
openArtifact({ id: "art-h2", version: 1, kind: "html", title: "h2", bytes: 1 });
equal(canvasSnapshot().tabs.map((t) => t.id), ["art-h2"], "a card click adds exactly that tab");

// A drawer the operator closed stays closed for the turn, even for a new artifact.
resetCanvas();
feed([card("art-old", 1, "m0", { live: false })], "m1");
openArtifact({ id: "art-old", version: 1, kind: "html", title: "old", bytes: 1 });
closeDrawer();
feed([card("art-old", 1, "m0", { live: false }), card("art-a", 1, "m1")]);
equal(canvasSnapshot().open, false, "closed for this turn");
equal(canvasSnapshot().tabs.length, 1, "no extra tab while closed");
// ...but the next turn opens again.
feed([card("art-old", 1, "m0", { live: false }), card("art-a", 1, "m1"), card("art-b", 1, "m2")], "m2");
equal([canvasSnapshot().open, canvasSnapshot().active], [true, "art-b"], "next turn opens");

// History and turns the operator did not start show a card only.
resetCanvas();
feed([card("art-h", 1, "m0", { live: false })]);
equal(canvasSnapshot().open, false, "history never opens");
feed([card("art-h", 1, "m0", { live: false }), card("art-p", 1, "m9", { operatorTurn: false })]);
equal(canvasSnapshot().open, false, "a peer's artifact never opens");
openArtifact({ id: "art-p", version: 1, kind: "html", title: "p", bytes: 1 });
equal([canvasSnapshot().open, canvasSnapshot().active], [true, "art-p"], "the card opens it");

// Tabs: reopening focuses, closing the active selects a neighbour, the last collapses.
openArtifact({ id: "art-h", version: 1, kind: "html", title: "h", bytes: 1 });
equal(canvasSnapshot().tabs.length, 2, "no duplicate tab");
closeTab("art-h");
equal(canvasSnapshot().active, "art-p", "neighbour selected");
closeTab("art-p");
equal([canvasSnapshot().open, canvasSnapshot().tabs.length], [false, 0], "last tab collapses");

// A conversation switch closes the drawer and forgets the tabs.
resetCanvas();
feed([card("art-a", 1, "m1")]);
feed([], "m1", "c2");
equal([canvasSnapshot().open, canvasSnapshot().tabs.length], [false, 0], "switch resets");

// Geometry: the chat keeps its room on each surface; cover never lies over it.
equal(drawerLayout(1440, 480, "home", null), { mode: "push", width: 480 }, "home pushes at desktop");
equal(drawerLayout(1440, 480, "app", 700), { mode: "push", width: 480 }, "app pushes at desktop");
equal(drawerLayout(900, 480, "home", null), { mode: "push", width: 480 }, "home still pushes at 900 (chat keeps 420)");
// Tablet app shell: the chat pane is narrowed to 352px, the drawer covers beside it.
equal(drawerLayout(900, 480, "app", 860), { mode: "cover", width: 480 }, "app at 900 covers beside the narrowed chat");
equal(drawerLayout(768, 480, "app", 752), { mode: "cover", width: 388 }, "app at 768 covers with what is left of the chat");
equal(drawerLayout(639, 480, "app", 623).mode, "sheet", "a phone gets the sheet");
equal(drawerLayout(660, 480, "app", 644).mode, "sheet", "too narrow to keep 320px of chat beside 360px of canvas: sheet");
equal(drawerLayout(1100, 660, "app", 700), { mode: "cover", width: 388 }, "app cover stops at the chat's right edge");
equal(drawerLayout(1100, 660, "home", null), { mode: "cover", width: 544 }, "home cover keeps the chat's first 320px");
equal(drawerLayout(375, 480, "home", null).mode, "sheet", "phone sheet");
// No chat pane in the page (navigated away from the app): never a desktop sheet.
equal(drawerLayout(1280, 480, "app", null), { mode: "cover", width: 480 }, "app, no chat pane, 1280 covers");
equal(drawerLayout(900, 480, "app", null), { mode: "cover", width: 480 }, "app, no chat pane, 900 covers");
equal(drawerLayout(1100, 660, "app", null), { mode: "cover", width: 544 }, "app, no chat pane, narrow room covers by the page rule");
// Default widths scale with the viewport, wider for html/csv/pdf, within their clamps.
equal([defaultWidth("md", 1835), defaultWidth("md", 1000), defaultWidth("md", 800)], [760, 480, 480], "default width 45%, 480..760");
equal([defaultWidth("html", 1835), defaultWidth("csv", 1280), defaultWidth("pdf", 900)], [960, 704, 560], "document kinds 55%, 560..960");
// Expand: cover taking most of the page, leaving a ~64px chat strip. The app shell keeps the chat pane's edge when that gains 96px or more; otherwise it covers the pane down to the strip.
equal(drawerLayout(1440, 480, "home", null, true), { mode: "cover", width: 1136 }, "home expands beside a chat strip");
equal(drawerLayout(1440, 480, "app", 700, true), { mode: "cover", width: 728 }, "app expand keeps the chat pane edge when it gains 96px or more");
equal(drawerLayout(900, 480, "app", 860, true).mode, "cover", "app expand at 900 stays beside the narrowed chat");
equal(drawerLayout(500, 480, "home", null, true).mode, "sheet", "a phone is a sheet either way");
// Expand never does nothing: where the chat pane's edge leaves no more room than cover has, it covers the chat down to the strip.
const crmCover = drawerLayout(1280, 704, "app", 640);
const crmExpand = drawerLayout(1280, 704, "app", 640, true);
equal([crmCover.width, crmExpand.width > crmCover.width + 96], [628, true], "expand widens a drawer already at the chat pane's edge (1280)");
equal(drawerLayout(900, 560, "app", 860, true).width > drawerLayout(900, 560, "app", 860).width, true, "expand widens at 900 too");
// A document kind keeps its 560px floor at 900; the cap follows the floor.
equal([defaultWidthFor("csv", 900), defaultWidthFor("md", 900), defaultWidthFor("html", 1280)], [560, 480, 704], "default widths hold their floors at 900");
equal(clampWidth(100, 900, 560), 560, "a floor lifts the minimum");
equal([clampWidth(100, 1440), clampWidth(2000, 1440), clampWidth(500, 1440)], [360, 864, 500], "width clamps to 360px..60%");

// A quoted-empty record is a record; blank lines and the trailing newline are not.
equal(parseCsv('h\r\n""\r\nx\r\n'), { header: ["h"], rows: [[""], ["x"]], total: 2, columns: 1 }, "a quoted-empty row is kept");
equal(parseCsv('""\r\nx\r\n'), { header: [""], rows: [["x"]], total: 1, columns: 1 }, "a quoted-empty header is kept");
equal(parseCsv("a\n\nb\n"), { header: ["a"], rows: [["b"]], total: 1, columns: 1 }, "blank lines are skipped");

// The feed reads cards and the live step from the thread, nothing else.
const e = (seq: number, role: string, kind: string, text: string, message: string, payload: Record<string, unknown> | null = null): ThreadEntry => ({
  seq, role, kind, text, payload, message,
});
const state: ThreadState = {
  exists: true,
  entries: [
    e(1, "operator", "message", "report please", "m1"),
    e(2, "agent", "assistant_text", "Pulling the numbers", "m1"),
    e(3, "agent", "tool_call", "Bash: cadence status", "m1", { tool_use_id: "t1" }),
    e(4, "agent", "artifact", "Artifact", "m1", { id: "art-a", version: 2, kind: "html", title: "Report", bytes: 5, digest: "d" }),
    e(5, "agent", "artifact", "Artifact", "m1", { id: "bad", version: 1, kind: "html", title: "x", bytes: 5 }),
  ],
  pending: [],
};
const parts = feedParts(threadItems(state), 3, "m1");
equal(parts.cards.map((c) => [c.key, c.live, c.operatorTurn, c.turn]), [["art-a@2", true, true, "m1"]], "malformed ids are dropped");
equal(
  parts.working,
  { plan: "Pulling the numbers", step: "Checking the board", steps: [{ text: "Checking the board", raw: "Bash: cadence status", done: false }], count: 1, turn: "m1", updating: [] },
  "plan, live step in plain words, no invented total",
);

// The Working view never shows a command line, an absolute path or JSON.
for (const raw of [
  "Bash: cadence artifact put --kind md --title x --file /home/ubuntu/.local/state/cadence/master/tmp/r.md",
  "Bash: cat /home/ubuntu/.local/state/cadence/master/tmp/a.json",
  'Read: /home/ubuntu/x {"bytes":4,"digest":"d"}',
  "weird",
]) {
  const plain = plainStep(raw);
  if (/bash:|\/home|\.local\/state|master\/tmp|[{}]|--/i.test(plain)) throw new Error(`plain step leaks: ${plain}`);
}
// The agent's own words keep their sentence but lose every path.
const planOf = (text: string) =>
  feedParts(threadItems({ exists: true, pending: [], entries: [e(1, "operator", "message", "go", "mp"), e(2, "agent", "assistant_text", text, "mp")] }), 0, "mp").working?.plan;
for (const leak of [
  "Writing to master/tmp/r.md first",
  "Saved at ~/.local/state/cadence/master/tmp/r.md",
  "path=/home/u/x",
  "--file=/home/u/x",
  "file:/home/u/x",
  "Using ./../tmp/x now",
  "see /home/u/x and (/tmp/y)",
]) {
  const plan = planOf(leak) ?? "";
  if (/\/|~|\.local|master/.test(plan)) throw new Error(`plan leaks: ${plan}`);
}
// Scrubbing stays linear: long runs of `:` or `a=` finish at once.
for (const hostile of [":".repeat(200_000), "a=".repeat(100_000)]) {
  const started = performance.now();
  planOf(hostile);
  if (performance.now() - started > 100) throw new Error("plan scrub is not linear");
}
equal(planOf("a:b:c/d and x=1:2"), "… and x=1:2", "key=value without a path is kept");
equal(planOf("Pulling the Q3 numbers, then summarising and/or charting them."), "Pulling the Q3 numbers, then summarising and/or charting them.", "a normal sentence survives intact");
// The agent's own commentary never carries a command line or inline JSON.
equal(planOf('Done.\nbash: cadence status\n$ ls\nBASH: x\nResult {"bytes":9,"digest":"d"} ok'), "Done.\nResult … ok", "command lines and inline JSON go");
for (const hostile of ["{".repeat(200_000), '{"a":'.repeat(40_000), " ".repeat(200_000) + "bash:"]) {
  const started = performance.now();
  planOf(hostile);
  if (performance.now() - started > 100) throw new Error("plan scrub is not linear");
}

// Pretty-printed and spaced JSON never reaches the strip; ordinary brackets do.
equal(planOf('Result:\n{\n  "bytes": 9,\n  "digest": "d"\n}\nAll set.'), "Result:\n…\nAll set.", "multiline JSON goes");
equal(planOf('Got {"a": 1, "b": 2} back'), "Got … back", "spaced compact JSON goes");
equal(planOf('Result: {"text": "Use } here", "bytes": 9, "digest": "d"}'), "Result: …", "a } inside a string does not end the span");
equal(planOf('Got {"a": "x ] y", "b": 1} ok'), "Got … ok", "a ] inside a string");
equal(planOf('Got {"a": "x { y", "b": 1} ok'), "Got … ok", "a { inside a string");
equal(planOf('Got {"a": "say \\"hi\\"", "b": 1} ok'), "Got … ok", "an escaped quote before the closing brace");
equal(planOf('{\n  "bytes" : 9,\n  "digest" : "d"\n}'), "…", "spaces before the colon");
equal(planOf('Got {"bytes"\t: 9} ok'), "Got … ok", "a tab before the colon");
equal(planOf("See [note], {braces} and [1, 2, 3] here"), "See [note], {braces} and [1, 2, 3] here", "prose brackets stay");
equal(planOf("Steps [1] and (2) done"), "Steps [1] and (2) done", "plain brackets stay");
for (const hostile of ["{ ".repeat(100_000), '{"a": '.repeat(40_000), "[".repeat(200_000), '"{'.repeat(100_000), '\\"'.repeat(100_000), "{" + '"'.repeat(200_000)]) {
  const started = performance.now();
  planOf(hostile);
  if (performance.now() - started > 100) throw new Error("plan scrub is not linear");
}

// Real daemon order: call, then the card (written inside the put RPC), then the
// result. The card must not split the call from its result.
const realOrder = threadItems({
  exists: true,
  pending: [],
  entries: [
    e(1, "operator", "message", "report", "m1"),
    e(2, "agent", "tool_call", "bash: cadence artifact put --kind md --title x --file ./../tmp/r.md", "m1", { tool_use_id: "t1" }),
    e(3, "agent", "artifact", "Artifact", "m1", { id: "art-a", version: 1, kind: "md", title: "x", bytes: 5 }),
    e(4, "agent", "tool_result", '{"bytes":5,"digest":"d"}', "m1", { tool_use_id: "t1" }),
  ],
});
equal(realOrder.map((i) => i.type), ["operator", "tools", "artifact"], "the card waits for the put's result");
const runSteps = realOrder[1].type === "tools" ? toolSteps(realOrder[1].entries) : [];
equal([runSteps.length, runSteps[0]?.done, runSteps[0] ? isArtifactPutCall(runSteps[0].call.text) : null], [1, true, true], "one paired put step");
equal([isArtifactPutCall("Bash: cadence help artifact put"), isArtifactPutCall("Bash: grep artifact put notes.md")], [false, false], "only a real put call matches");

// A follow-up queued behind the running turn does not take over the strip.
const queued = threadItems({
  exists: true,
  pending: [],
  entries: [
    e(1, "operator", "message", "tighten it", "mA"),
    e(2, "agent", "tool_call", "bash: cadence artifact put --kind md --title x --id art-a --file ./../tmp/r.md", "mA", { tool_use_id: "t1" }),
    e(3, "operator", "message", "and also this", "mB"),
    e(4, "agent", "artifact", "Artifact", "mA", { id: "art-a", version: 2, kind: "md", title: "x", bytes: 5 }),
  ],
});
const queuedParts = feedParts(queued, 0, "mA");
equal([queuedParts.turn, queuedParts.working?.turn], ["mA", "mA"], "the running message owns the strip");
resetCanvas();
openArtifact({ id: "art-a", version: 1, kind: "md", title: "x", bytes: 5 });
feedCanvas({ conversation: "c1", surface: "home", composerKey: "home", ...queuedParts });
equal(canvasSnapshot().tabs[0].turn, queuedParts.working?.turn, "the landed put matches the running turn, so no stale updating chip");
equal(plainStep("bash: cadence artifact put --kind md --id art-a --file ./../tmp/r.md"), "Putting it in the canvas", "a put reads plainly");

// "updating vN" only names artifacts the turn asked to add a version to.
const putState: ThreadState = {
  exists: true,
  pending: [],
  entries: [
    e(1, "operator", "message", "tighten it", "m1"),
    e(2, "agent", "tool_call", "Bash: cadence artifact put --kind md --title x --id art-a --file ./../tmp/r.md", "m1", { tool_use_id: "t9" }),
  ],
};
equal(feedParts(threadItems(putState), 0, "m1").working?.updating, ["art-a"], "the put names its target");
equal(feedParts(threadItems(putState), 0, null).working, null, "no strip when no turn runs");
// The global-flags form is a put too; a landed put is no longer "updating"; a second put names its own target.
for (const text of ["Bash: cadence --org local artifact put --kind md", "cadence --json --state-dir /x/y artifact put --kind md", "bash: /usr/bin/cadence --org=local artifact put --kind md"]) {
  equal(isArtifactPutCall(text), true, `put with global flags: ${text}`);
}
equal(["cadence --org local issue ls artifact put", "cadence issue show artifact put", "cadence artifact ls"].map(isArtifactPutCall), [false, false, false], "other commands are not puts");
const twoPuts: ThreadState = {
  exists: true,
  pending: [],
  entries: [
    e(1, "operator", "message", "two", "m1"),
    e(2, "agent", "tool_call", "Bash: cadence artifact put --id art-a --kind md --file ./../tmp/a.md", "m1", { tool_use_id: "t1" }),
    e(3, "agent", "tool_result", "ok", "m1", { tool_use_id: "t1" }),
    e(4, "agent", "tool_call", "Bash: cadence artifact put --id art-a --kind md --file ./../tmp/b.md", "m1", { tool_use_id: "t2" }),
  ],
};
equal(feedParts(threadItems(twoPuts), 0, "m1").working?.updating, ["art-a"], "the second, still-open put is the one updating");
// A command quoted mid-sentence leaves the plan line.
const quoted: ThreadState = {
  exists: true,
  pending: [],
  entries: [
    e(1, "operator", "message", "go", "m1"),
    e(2, "agent", "assistant_text", 'Next I will run "cadence artifact put --kind md --title x" and then `cadence status --json` for counts.', "m1"),
  ],
};
equal(feedParts(threadItems(quoted), 0, "m1").working?.plan, "Next I will run … and then … for counts.", "quoted commands are elided");
// Linear on 200k-character runs (a quadratic regex would not finish).
const probeStart = Date.now();
const longRuns = ["\"".repeat(200000), "\"".repeat(100000) + "/".repeat(100000), "`".repeat(100000) + "cadence x", "(".repeat(100000), "/".repeat(200000), "=".repeat(200000), "plan show ".repeat(20000), "plan show " + "A".repeat(200000), `plan show ${"x ".repeat(100000)}`, "cadence " + "--a ".repeat(50000) + "artifact put", `"cadence ${"a ".repeat(100000)}`];
for (const run of longRuns) {
  planRef({ ...e(1, "agent", "tool_call", run, "m1") });
  isArtifactPutCall(run);
  feedParts(threadItems({ exists: true, pending: [], entries: [e(1, "operator", "message", "go", "m1"), e(2, "agent", "assistant_text", run, "m1")] }), 0, "m1");
}
equal(Date.now() - probeStart < 3000, true, "200k-character runs stay linear");
equal([planRef(e(1, "agent", "tool_call", "Bash: cadence plan show CAD-12", "m1")), planRef(e(1, "agent", "tool_call", "plan show x\nplan approve EPIC-3 now", "m1")), planRef(e(1, "agent", "tool_call", "plan show", "m1"))], ["CAD-12", "EPIC-3", null], "plan commands still name their epic");

// CSV: RFC 4180 quoting, a header row, a row cap, and a refusal to guess.
equal(
  parseCsv('a,b\r\n"x, y","say ""hi"""\n"two\nlines",3\n'),
  { header: ["a", "b"], rows: [["x, y", 'say "hi"'], ["two\nlines", "3"]], total: 2, columns: 2 },
  "quoted fields",
);
equal(parseCsv('a,b\n"open,1'), null, "an unterminated quote falls back to text");
equal(parseCsv('a,b\n"x"y,1'), null, "text after a closing quote falls back to text");
const big = parseCsv(`h\n${Array.from({ length: 2500 }, (_, i) => i).join("\n")}`);
equal([big?.rows.length, big?.total], [2000, 2500], "at most 2,000 rows, total reported");
const wide = parseCsv(`${Array.from({ length: 80 }, (_, i) => `h${i}`).join(",")}\n${Array.from({ length: 80 }, (_, i) => i).join(",")}`);
equal([wide?.header.length, wide?.rows[0].length, wide?.columns], [50, 50, 80], "at most 50 columns, count reported");

// Ask to change: the chip expands into the body the master receives.
const ask = makeAskRef({ artifact: "art-a", version: 2, title: "Report", passage: "Tuesday lunch is soft" });
equal(ask.kind, "artifact", "ref kind");
equal(askLabel(ask), "“Tuesday lunch is soft” · v2", "chip label");
equal(
  askBody("make it softer", [ask]),
  'About artifact art-a (version 2) "Report", this passage:\n> Tuesday lunch is soft\n\nmake it softer',
  "body carries id, version and passage",
);
