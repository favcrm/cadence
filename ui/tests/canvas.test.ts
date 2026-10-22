import {
  canvasMode,
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
  feedCanvas({ conversation, composerKey: "home", cards, working: null, turn });

// The first live artifact of an operator-started turn opens the drawer; a
// later version of it switches the frame without reopening anything.
resetCanvas();
feed([card("art-a", 1, "m1")]);
equal([canvasSnapshot().open, canvasSnapshot().active], [true, "art-a"], "first artifact opens");
feed([card("art-a", 1, "m1"), card("art-a", 2, "m1")]);
equal(canvasSnapshot().tabs[0].selected, 2, "v2 followed");
selectVersion("art-a", 1);
feed([card("art-a", 1, "m1"), card("art-a", 2, "m1"), card("art-a", 3, "m1")]);
equal([canvasSnapshot().tabs[0].selected, canvasSnapshot().tabs[0].latest], [1, 3], "an older pick is kept");

// A drawer the operator closed stays closed for the turn, even for a new artifact.
resetCanvas();
feed([card("art-old", 1, "m0", { live: false })], "m1");
openArtifact({ id: "art-old", version: 1, kind: "html", title: "old", bytes: 1 });
closeDrawer();
feed([card("art-old", 1, "m0", { live: false }), card("art-a", 1, "m1")]);
equal(canvasSnapshot().open, false, "closed for this turn");
equal(canvasSnapshot().tabs.length, 2, "the card is still a tab");
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

// Geometry.
equal(canvasMode(1440, 480), "push", "desktop pushes");
equal(canvasMode(900, 480), "cover", "narrow covers");
equal(canvasMode(375, 480), "sheet", "phone sheet");
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
equal(parts.working, { plan: "Pulling the numbers", step: "Bash: cadence status" }, "plan and live step");
