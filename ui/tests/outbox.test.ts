import {
  outboxHref,
  postTitle,
  publicationTime,
  readOutboxFilters,
  visiblePosts,
} from "../src/features/outbox/outboxView";
import type { OutboxItem } from "../src/lib/types";

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}
const rows: OutboxItem[] = [
  {
    effect_id: "old",
    project: "site",
    title: "Autumn",
    published_at: "2025-09-27T10:00:00Z",
    preview: "Campaign draft",
  },
  {
    effect_id: "b",
    project: "website",
    title: "Autumn campaign",
    published_at: "2026-09-27T10:00:00Z",
  },
  {
    effect_id: "a",
    project: "site",
    title: "Launch",
    published_at: "2026-09-27T10:00:00Z",
    attachments: [{ name: "Autumn-creative.png", bytes: 0, sha256: "abc" }],
  },
  {
    effect_id: "invalid",
    project: "site",
    title: " ",
    published_at: "unknown",
  },
];
assert(
  visiblePosts(rows, "", "")
    .map((row) => row.effect_id)
    .join() === "a,b,old,invalid",
  "newest first, deterministic ties, unknown date last",
);
assert(rows[0].effect_id === "old", "sorting does not mutate the collection");
assert(
  visiblePosts(rows, "AUTUMN creative", "site")[0]?.effect_id === "a",
  "all search terms match case-insensitively across attachment names",
);
assert(
  visiblePosts(rows, "autumn campaign", "site")
    .map((row) => row.effect_id)
    .join() === "old",
  "project is exact, not a substring or alias guess",
);
assert(
  visiblePosts(rows, "launch impossible", "").length === 0,
  "all terms required",
);
assert(
  visiblePosts(rows, "", "missing").length === 0,
  "unknown project never silently becomes All",
);
assert(
  postTitle(rows[3]) === "Untitled post",
  "blank titles retain an actionable name",
);
assert(
  publicationTime(rows[0].published_at).includes("2025") &&
    publicationTime(rows[0].published_at).endsWith("UTC"),
  "date keeps year and timezone",
);
assert(
  publicationTime("unknown") === "unknown" &&
    publicationTime("") === "Date unavailable",
  "unknown dates are not invented",
);
const open = outboxHref(
  "outbox_q=creative&outbox_project=site&source=app&issue=CAD-1",
  { item: "effect/a + b" },
);
const parsed = readOutboxFilters(open.split("?")[1]);
assert(
  parsed.item === "effect/a + b" &&
    parsed.query === "creative" &&
    parsed.project === "site",
  "encoded identity and filters survive opening",
);
const back = outboxHref(open.split("?")[1], { item: null });
assert(
  !back.includes("item=") &&
    back.includes("outbox_q=creative") &&
    back.includes("source=app") &&
    back.includes("issue=CAD-1"),
  "back drops only item",
);
assert(
  outboxHref("outbox_q=x&outbox_project=site&item=a&source=app", {
    query: "",
    project: "",
  }) === "/outbox?item=a&source=app",
  "clear removes only owned filters",
);
console.log(
  "outbox: ordering, exact scope, search, date and navigation passed",
);
