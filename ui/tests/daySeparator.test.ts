/** CAD-1886: the transcript's day rule. */
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { LocaleProvider } from "../src/lib/locale";
import { DayDivider, dayKey, dayText, itemCreated } from "../src/features/home/DayDivider";
import { threadItems } from "../src/features/home/thread";

function ok(cond: boolean, what: string): void {
  if (!cond) throw new Error(`failed: ${what}`);
}

const DAY = 86_400_000;
const now = new Date(2026, 9, 27, 15, 0, 0).getTime();
const translate = (key: string) => key;

const today = dayText(now, now, translate);
const yesterday = dayText(now - DAY, now, translate);
const older = dayText(now - 5 * DAY, now, translate);
ok(today === "Today", `today reads Today (got ${today})`);
ok(yesterday === "Yesterday", `yesterday reads Yesterday (got ${yesterday})`);
ok(older !== "Today" && older !== "Yesterday" && older.length > 0, `older reads a date (got ${older})`);

// Same local day at a different hour is one key; the next day is another.
ok(dayKey(now) === dayKey(new Date(2026, 9, 27, 23, 59).getTime()), "same local day keeps one key");
ok(dayKey(now) !== dayKey(now - DAY), "the previous day is a different key");

const createdIso = new Date(now - DAY).toISOString();
const [item] = threadItems({
  exists: true,
  conversation: "c-1",
  entries: [{ seq: 1, role: "agent", kind: "assistant_text", text: "note", message: "m-1", created: createdIso }],
  pending: [],
});
ok(itemCreated(item) === now - DAY, "itemCreated reads an ISO instant");
ok(itemCreated(item) !== null && dayKey(itemCreated(item)!) === dayKey(now - DAY), "the item lands on yesterday's key");

const html = renderToStaticMarkup(
  createElement(LocaleProvider, null, createElement(DayDivider, { ms: now, now })),
);
ok(html.includes('data-day-sep'), "the divider carries its test hook");
ok(html.includes("Today"), "the divider prints Today");

console.log("day separator checks passed");
