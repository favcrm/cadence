/* ScLibrary — source posts grid. Clicking a card opens a side drawer to
 * inspect the post (media carousel, full caption, stats, drafts made from
 * it); the corner checkbox keeps the multi-select for "Draft N posts".
 * Drawer keys: Esc closes, ← → move between posts. */
import { useState } from "react";
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { Media, PlatformChip, StatusChip, BTN_SM, BTN_PRIMARY_SM, PageHead } from "./widgets";
import ScDrawer from "./ScDrawer";
import { navigate } from "../../../lib/useLocation";
import type { SourcePost } from "./mock/data";

function MediaCarousel({ s }: { s: SourcePost }) {
  const [i, setI] = useState(0);
  const n = s.media.length;
  if (!n)
    return (
      <div className="sc-caritem">
        <Media asset={null} />
      </div>
    );
  const cur = s.media[Math.min(i, n - 1)];
  return (
    <div className="sc-caro">
      <div className="sc-caritem">
        <Media asset={cur.seed} />
        {n > 1 && (
          <>
            <button className="sc-carnav sc-prev" aria-label="previous image"
              onClick={() => setI((v) => (v - 1 + n) % n)}>‹</button>
            <button className="sc-carnav sc-next" aria-label="next image"
              onClick={() => setI((v) => (v + 1) % n)}>›</button>
            <span className="sc-carnum num">{Math.min(i, n - 1) + 1}/{n}</span>
          </>
        )}
      </div>
      {n > 1 && (
        <div className="sc-cardots">
          {s.media.map((_, k) => (
            <button key={k} className={`sc-cardot ${k === i ? "sc-on" : ""}`}
              aria-label={`image ${k + 1}`} onClick={() => setI(k)} />
          ))}
        </div>
      )}
    </div>
  );
}

function SourceDrawer({
  s,
  say,
  postHref,
  runsHref,
  onClose,
  onPrev,
  onNext,
}: {
  s: SourcePost;
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  postHref: (id: string) => string;
  runsHref: string;
  onClose: () => void;
  onPrev?: () => void;
  onNext?: () => void;
}) {
  const c = api.clients.current();
  const drafts = api.posts.fromSource(s.id);
  return (
    <ScDrawer
      title={(s.text || "source post").slice(0, 90)}
      sub={`${s.platform} · @${c.handle} · ${fmt.ago(s.postedAt)}`}
      onClose={onClose}
      onPrev={onPrev}
      onNext={onNext}
    >
      <MediaCarousel key={s.id} s={s} />

      <section className="card p-3">
        <div className="slabel mb-1.5">caption</div>
        <div className="text-secondary text-ink-200 whitespace-pre-wrap">{s.text}</div>
      </section>

      <section className="card p-3">
        <div className="sc-kv">
          <span className="sc-k">platform</span>
          <span><PlatformChip pl={s.platform} /></span>
          <span className="sc-k">account</span>
          <span className="text-label text-ink-200">@{c.handle} — {c.name}</span>
          <span className="sc-k">posted</span>
          <span className="text-label text-ink-200">{fmt.day(s.postedAt)} · {fmt.ago(s.postedAt)}</span>
          <span className="sc-k">source</span>
          <a className="lnk text-label break-all" href={s.url} target="_blank" rel="noreferrer">
            {s.url.replace(/^https?:\/\//, "")}
          </a>
          <span className="sc-k">stats</span>
          <span className="num text-label text-ink-200">
            ♥ {s.stats.likes} · 💬 {s.stats.comments}
          </span>
        </div>
      </section>

      <section className="card p-3 flex flex-col gap-2">
        <div className="slabel">drafts made from this · {drafts.length}</div>
        {drafts.length ? (
          drafts.map((p) => (
            <div key={p.id} className="flex items-center gap-2.5">
              <StatusChip s={p.status} />
              <span className="text-label text-ink-400 truncate min-w-0 flex-1">
                {p.caption.text || "(no caption yet)"}
              </span>
              <Link href={postHref(p.id)} className="lnk text-label shrink-0">open</Link>
            </div>
          ))
        ) : (
          <div className="text-label text-ink-500">none yet — draft it below</div>
        )}
      </section>

      <div className="flex items-center gap-2 pb-1">
        <button
          className={BTN_PRIMARY_SM}
          onClick={() => {
            const rid = api.runs.draft([s.id]);
            say("ok", `${rid} started — drafting 1 post. See Runs.`);
            onClose();
            navigate(runsHref);
          }}
        >
          Draft from this
        </button>
        <button
          className={BTN_SM}
          onClick={() => {
            api.sources.hide(s.id);
            say("ok", `${s.id} hidden — out of the Library`);
            onClose();
          }}
        >
          Skip / hide
        </button>
      </div>
    </ScDrawer>
  );
}

export default function ScLibrary({
  say,
  postHref,
  runsHref,
}: {
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  postHref: (id: string) => string;
  runsHref: string;
}) {
  const [newOnly, setNewOnly] = useState(true);
  const [openId, setOpenId] = useState<string | null>(null);
  const list = api.sources.list();
  const sel = new Set(api.sources.selectedIds());
  const shown = newOnly ? list.filter((s) => s.isNew) : list;
  const idx = shown.findIndex((s) => s.id === openId);
  const open = idx >= 0 ? shown[idx] : null;

  const draft = (ids: string[]) => {
    const rid = api.runs.draft(ids);
    say("ok", `${rid} started — drafting ${ids.length} post(s). See Runs.`);
    navigate(runsHref);
  };

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Library"
        note={`source posts — ${api.clients.current().name}`}
      />
      <div className="flex items-center gap-2.5 mb-3 flex-wrap">
        <button
          className={newOnly ? BTN_PRIMARY_SM : BTN_SM}
          onClick={() => setNewOnly((v) => !v)}
        >
          new only{newOnly ? " ✓" : ""}
        </button>
        <span className="text-label text-ink-500">
          {sel.size ? `${sel.size} selected` : `${shown.length} of ${list.length} shown`}
        </span>
        <span className="flex-1" />
        {sel.size > 0 && (
          <button className={BTN_SM} onClick={() => api.sources.clear()}>
            clear
          </button>
        )}
        <button
          className={BTN_PRIMARY_SM}
          disabled={!sel.size}
          onClick={() => draft(api.sources.selectedIds())}
        >
          {sel.size ? `Draft ${sel.size} post${sel.size > 1 ? "s" : ""}` : "Draft posts"}
        </button>
      </div>
      <div className="text-label text-ink-500 mb-3">
        Click a card to inspect it; the corner box selects for a batch draft.
      </div>

      <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(230px, 1fr))" }}>
        {shown.map((s) => (
          <div
            key={s.id}
            className={`card tcard sc-src ${sel.has(s.id) ? "sc-sel" : ""}`}
            role="button"
            tabIndex={0}
            onClick={() => setOpenId(s.id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                setOpenId(s.id);
              }
            }}
          >
            <div className="sc-thumb">
              <Media asset={s.media[0]?.seed} />
              <button
                className={`sc-pick ${sel.has(s.id) ? "sc-on" : ""}`}
                aria-pressed={sel.has(s.id)}
                aria-label={`select ${s.id}`}
                title={sel.has(s.id) ? "selected" : "select for batch draft"}
                onClick={(e) => {
                  e.stopPropagation();
                  api.sources.toggle(s.id);
                }}
              >
                {sel.has(s.id) ? "✓" : ""}
              </button>
            </div>
            <div className="sc-body">
              <div className="sc-txt">{s.text}</div>
              <div className="sc-meta">
                <PlatformChip pl={s.platform} />
                {s.isNew && (
                  <span className="chip bg-accent/10 text-accent">
                    <span className="sc-newdot" />
                    new
                  </span>
                )}
                <span className="sc-ago">{fmt.ago(s.postedAt)}</span>
              </div>
            </div>
          </div>
        ))}
      </div>

      {open && (
        <SourceDrawer
          key={open.id}
          s={open}
          say={say}
          postHref={postHref}
          runsHref={runsHref}
          onClose={() => setOpenId(null)}
          onPrev={idx > 0 ? () => setOpenId(shown[idx - 1].id) : undefined}
          onNext={idx < shown.length - 1 ? () => setOpenId(shown[idx + 1].id) : undefined}
        />
      )}
    </main>
  );
}
