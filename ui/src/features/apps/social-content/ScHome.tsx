/* ScHome — the app's Home: week calendar + status board + suggestions rail. */
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { PostCard, StatusDot, STATUS, SuggCard } from "./widgets";
import type { PostStatus } from "./mock/data";

const COLS: PostStatus[] = ["drafting", "in_review", "ready", "waiting", "scheduled", "published", "needs_you"];

export default function ScHome({ postHref }: { postHref: (id: string) => string }) {
  const posts = api.posts.list();
  const now = api.NOW();
  const days = fmt.weekOf(now.toISOString());
  const todayKey = fmt.dkey(now.toISOString());
  const suggs = api.suggestions.list();

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9 grid gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,19rem)] lg:items-start">
      <div className="min-w-0">
        <section className="mb-5">
          <div className="slabel mb-2">this week</div>
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

        <section>
          <div className="slabel mb-2">board</div>
          <div className="sc-board">
            {COLS.map((s) => {
              const inCol = posts.filter((p) => p.status === s);
              if (s === "needs_you" && !inCol.length) return null;
              return (
                <div key={s} className="sc-bcol min-w-0">
                  <div className="sc-bhead">
                    <span className="slabel">
                      <StatusDot s={s} />
                      {STATUS[s].label}
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
        </section>
      </div>

      <aside className="flex flex-col gap-2.5 lg:sticky lg:top-[3.6rem]">
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
    </main>
  );
}
