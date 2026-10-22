import {
  drawerLayout,
  canvasSnapshot,
  clampWidth,
  closeDrawer,
  closeTab,
  feedCanvas,
  openArtifact,
  resetCanvas,
  selectVersion,
  type FeedCard,
} from "../src/features/canvas/canvasStore";
import { feedParts } from "../src/features/canvas/feed";
import { askBody, askLabel, makeAskRef } from "../src/features/canvas/askRefs";
import { threadItems, type ThreadEntry, type ThreadState } from "../src/features/home/thread";

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
equal(drawerLayout(900, 480, "app", 860).mode, "sheet", "app at 900 with a full-width chat falls to the sheet");
equal(drawerLayout(900, 480, "app", 500), { mode: "cover", width: 388 }, "app cover stops at the chat's right edge");
equal(drawerLayout(1100, 660, "home", null), { mode: "cover", width: 544 }, "home cover keeps the chat's first 320px");
equal(drawerLayout(375, 480, "home", null).mode, "sheet", "phone sheet");
// No chat pane in the page (navigated away from the app): never a desktop sheet.
equal(drawerLayout(1280, 480, "app", null), { mode: "cover", width: 480 }, "app, no chat pane, 1280 covers");
equal(drawerLayout(900, 480, "app", null), { mode: "cover", width: 480 }, "app, no chat pane, 900 covers");
equal(drawerLayout(1100, 660, "app", null), { mode: "cover", width: 544 }, "app, no chat pane, narrow room covers by the page rule");
equal([clampWidth(100, 1440), clampWidth(2000, 1440), clampWidth(500, 1440)], [360, 864, 500], "width clamps to 360px..60%");

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
const parts = feedParts(threadItems(state), 3, true);
equal(parts.cards.map((c) => [c.key, c.live, c.operatorTurn, c.turn]), [["art-a@2", true, true, "m1"]], "malformed ids are dropped");
equal(parts.working, { plan: "Pulling the numbers", step: "Bash: cadence status", steps: [{ text: "Bash: cadence status", done: false }], count: 1, turn: "m1" }, "plan, live step, no invented total");

// Ask to change: the chip expands into the body the master receives.
const ask = makeAskRef({ artifact: "art-a", version: 2, title: "Report", passage: "Tuesday lunch is soft" });
equal(ask.kind, "artifact", "ref kind");
equal(askLabel(ask), "“Tuesday lunch is soft” · v2", "chip label");
equal(
  askBody("make it softer", [ask]),
  'About artifact art-a (version 2) "Report", this passage:\n> Tuesday lunch is soft\n\nmake it softer',
  "body carries id, version and passage",
);
