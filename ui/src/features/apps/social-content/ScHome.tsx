/* ScHome — the app's Home, two views under a segmented control:
 *   Calendar — this week's schedule, a short "needs you" summary and the
 *              suggestions rail (home stays light);
 *   Board    — the pipeline in its own container: lanes scroll horizontally
 *              inside it, headers stick, cards are compact (thumb, first
 *              caption line, destinations, time, state).
 */
import { useState } from "react";
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { PostCard, StatusDot, STATUS, SuggCard } from "./widgets";
import type { PostStatus } from "./mock/data";

/* The operator's five pipeline lanes. `ready` posts queue in "waiting"
 * (they're waiting for the digest — the card still says "ready") and
 * needs-you posts get a red lane at the end, only when it has cards. */
const LANES: { id: string; label: string; statuses: PostStatus[]; dot: PostStatus }[] = [
  { id: "drafting", label: "drafting", statuses: ["drafting"], dot: "drafting" },
  { id: "in_review", label: "in review", statuses: ["in_review"], dot: "in_review" },
  { id: "waiting", label: "waiting", statuses: ["ready", "waiting"], dot: "waiting" },
  { id: "scheduled", label: "scheduled", statuses: ["scheduled"], dot: "scheduled" },
  { id: "published", label: "published", statuses: ["published"], dot: "published" },
  { id: "needs_you", label: "needs you", statuses: ["needs_you"], dot: "needs_you" },
];

function Board({ postHref }: { postHref: (id: string) => string }) {
  const posts = api.posts.list();
  return (
    <div className="card sc-boardwrap">
      <div className="sc-board" role="region" aria-label="pipeline board">
        {LANES.map((lane) => {
          const inCol = posts.filter((p) => lane.statuses.includes(p.status));
          if (lane.id === "needs_you" && !inCol.length) return null;
          return (
            <div key={lane.id} className="sc-bcol min-w-0">
              <div className="sc-bhead">
                <span className="slabel">
                  <StatusDot s={lane.dot} />
                  {lane.label}
                </span>
                <span className="sc-count">{inCol.length}</span>
              </div>
              <div className="sc-bcards">
                {inCol.length ? (
                  inCol.map((p) => <PostCard key={p.id} p={p} href={postHref(p.id)} />)
                ) : (
                  <div className="text-label text-ink-500 px-1">—</div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default function ScHome({
  postHref,
  needsHref,
}: {
  postHref: (id: string) => string;
  needsHref: string;
}) {
  const [view, setView] = useState<"calendar" | "board">("calendar");
  const posts = api.posts.list();
  const now = api.NOW();
  const days = fmt.weekOf(now.toISOString());
  const todayKey = fmt.dkey(now.toISOString());
  const suggs = api.suggestions.list();
  const needs = api.needsYou.list();

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <div className="flex items-center gap-3 mb-4">
        <div className="sc-seg" role="tablist" aria-label="home view">
          {(["calendar", "board"] as const).map((v) => (
            <button
              key={v}
              role="tab"
              aria-selected={view === v}
              className={view === v ? "sc-on" : ""}
              onClick={() => setView(v)}
            >
              {v === "calendar" ? "Calendar" : "Board"}
            </button>
          ))}
        </div>
        <span className="kicker">
          {view === "calendar" ? "this week + what needs you" : "every post by stage"}
        </span>
      </div>

      {view === "board" ? (
        <Board postHref={postHref} />
      ) : (
        <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,19rem)] lg:items-start">
          <section className="min-w-0">
            <div className="sc-weekwrap">
              <div className="sc-week">
                {days.map((d) => {
                  const dkey = fmt.hkDayKey(d);
                  const dayPosts = posts
                    .filter((p) => fmt.dkey(p.scheduleAt) === dkey)
                    .sort((a, b) => (a.scheduleAt < b.scheduleAt ? -1 : 1));
                  const today = dkey === todayKey;
                  return (
                    <div key={dkey} className={`sc-daycol ${today ? "sc-today" : ""}`}>
                      <div className="sc-dayhead">
                        <span className="sc-dow">{fmt.day(d.toISOString()).split(" ")[0]}</span>
                        <span className="sc-dnum num">{Number(dkey.split("/")[0])}</span>
                      </div>
                      {dayPosts.map((p) => (
                        <Link
                          key={p.id}
                          href={postHref(p.id)}
                          className="sc-calpost"
                          style={{ borderLeftColor: STATUS[p.status].dot }}
                          title={p.caption.text || "(no caption)"}
                        >
                          <div className="sc-t">{fmt.time(p.scheduleAt).split(" ").pop()}</div>
                          <div className="sc-cap">{p.caption.text || "being drafted…"}</div>
                        </Link>
                      ))}
                    </div>
                  );
                })}
              </div>
            </div>
          </section>

          <aside className="flex flex-col gap-2.5 lg:sticky lg:top-[3.6rem]">
            {needs.length > 0 && (
              <Link href={needsHref} className="card tcard p-3 flex flex-col gap-1.5">
                <span className="slabel !text-fail">needs you · {needs.length}</span>
                {needs.slice(0, 3).map((n) => (
                  <span key={n.kind + (n.digest?.id ?? n.post?.id ?? "")} className="text-label text-ink-300">
                    {n.title}
                  </span>
                ))}
              </Link>
            )}
            <div className="slabel">suggestions · {suggs.length}</div>
            {suggs.length ? (
              suggs.map((s) => <SuggCard key={s.id} s={s} postHref={postHref} />)
            ) : (
              <div className="card p-3">
                <div className="text-label text-ink-500">
                  No pending proposals — when an agent wants to change a field you edited, it asks here.
                </div>
              </div>
            )}
          </aside>
        </div>
      )}
    </main>
  );
}
