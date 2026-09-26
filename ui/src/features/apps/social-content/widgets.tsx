/* widgets.tsx — shared pieces used by several social-content screens:
 * status chips, post cards, the IG-style preview, revision history,
 * suggestion cards. All read through the mock facade; none touches data.ts.
 */
import type { ReactNode } from "react";
import Link from "../../../ui/Link";
import { fmt, diff } from "./fmt";
import { api } from "./mock/api";
import type { Post, PostStatus, Suggestion } from "./mock/data";

/* Board-shaped controls (the class strings the rest of the board types out
 * inline — collected here so the app's screens stay readable). */
export const BTN_SM =
  "h-7 px-2.5 inline-flex items-center gap-1.5 rounded border border-ink-600 text-label text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-40";
export const BTN_PRIMARY_SM =
  "h-7 px-2.5 inline-flex items-center gap-1.5 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40";
export const CHIP_BASE = "chip bg-ink-800 text-ink-400";

export const STATUS: Record<PostStatus, { label: string; dot: string; chip: string }> = {
  drafting: { label: "drafting", dot: "var(--color-info)", chip: "bg-info/10 text-info" },
  in_review: { label: "in review", dot: "var(--color-warn)", chip: "bg-warn/10 text-warn" },
  ready: { label: "ready", dot: "var(--sc-violet)", chip: "bg-[color-mix(in_srgb,var(--sc-violet)_12%,transparent)] text-[var(--sc-violet)]" },
  waiting: { label: "waiting", dot: "var(--color-accent)", chip: "bg-accent/10 text-accent" },
  scheduled: { label: "scheduled", dot: "var(--color-info)", chip: "bg-info/10 text-info" },
  published: { label: "published", dot: "var(--color-ok)", chip: "bg-ok/15 text-ok" },
  needs_you: { label: "needs you", dot: "var(--color-fail)", chip: "bg-fail/10 text-fail" },
};

export function StatusDot({ s }: { s: PostStatus }) {
  return (
    <span
      className="inline-block w-[7px] h-[7px] rounded-full shrink-0"
      style={{ background: STATUS[s]?.dot ?? "var(--color-ink-500)" }}
    />
  );
}

export function StatusChip({ s }: { s: PostStatus }) {
  return <span className={`chip ${STATUS[s]?.chip ?? CHIP_BASE}`}>{STATUS[s]?.label ?? s}</span>;
}

/** Who wrote a field revision — "you" is accent, agents violet, system plain. */
export function ByChip({ by, via }: { by: string; via?: string }) {
  const cls = by === "you" ? "bg-accent/10 text-accent" : by === "system" ? CHIP_BASE : "bg-[color-mix(in_srgb,var(--sc-violet)_12%,transparent)] text-[var(--sc-violet)]";
  return <span className={`chip ${cls}`}>{by === "you" ? "you" : by + (via ? ` (${via})` : "")}</span>;
}

export function PlatformChip({ pl }: { pl: string }) {
  return <span className={`chip ${CHIP_BASE}`}>{pl}</span>;
}

/* placeholder media — deterministic gradient per seed, or a data: URL for
 * real uploads */
const PALETTES = [
  ["#3b2f2f", "#c2603e"], ["#1f3a3d", "#2dd4bf"], ["#2b2450", "#7c5cd6"],
  ["#402626", "#e05d44"], ["#20304a", "#60a5fa"], ["#3a2f1d", "#fbbf24"],
  ["#1d3428", "#4ade80"], ["#331f38", "#f472b6"],
];
export function Media({ asset }: { asset: string | null | undefined }) {
  if (!asset) {
    return (
      <div className="sc-phimg" style={{ background: "var(--color-ink-800)" }}>
        <span className="sc-glyph" style={{ opacity: 0.4 }}>no image</span>
      </div>
    );
  }
  if (asset.startsWith("data:")) {
    return (
      <div className="sc-phimg">
        <img src={asset} alt="" />
      </div>
    );
  }
  let n = 0;
  for (const c of asset) n = (n + c.charCodeAt(0)) % 997;
  const [c1, c2] = PALETTES[n % PALETTES.length];
  return (
    <div className="sc-phimg" style={{ background: `linear-gradient(135deg, ${c1}, ${c2})` }}>
      <div className="sc-pat" />
      <span className="sc-glyph">
        {asset.startsWith("gen-") || asset.startsWith("poster-") ? "✦ gen" : asset.replace(/-/g, " ")}
      </span>
    </div>
  );
}

/* The board's icons, plus the social glyphs the IG preview needs. */
export const ICONS = {
  heart: (
    <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d="M8 13.5s-5.5-3.4-5.5-7.4A3.1 3.1 0 0 1 8 4.2a3.1 3.1 0 0 1 5.5 1.9c0 4-5.5 7.4-5.5 7.4z" />
    </svg>
  ),
  comment: (
    <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d="M13.5 8a5.5 5.5 0 0 1-9 4.3L2.5 13.5l1.2-2A5.5 5.5 0 1 1 13.5 8z" />
    </svg>
  ),
  send: (
    <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d="M13.8 2.2 7 9M13.8 2.2l-4 11.6-2.8-4.8-4.8-2.8z" />
    </svg>
  ),
  bookmark: (
    <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d="M4 2.5h8V14L8 11l-4 3z" />
    </svg>
  ),
  check: (
    <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8">
      <path d="M3 8.5 6.5 12 13 4.5" />
    </svg>
  ),
};

/** Tiny platform glyph for a destination chip — "ig" / "fb" / "web". */
export function DestIcon({ d }: { d: string }) {
  const short = d === "instagram" ? "ig" : d === "facebook" ? "fb" : d.slice(0, 3);
  return (
    <span className="sc-dicon" title={d}>
      {short}
    </span>
  );
}

/** A post on the board — thumb, first caption line, destinations, time,
 * state; opens the editor. */
export function PostCard({ p, href }: { p: Post; href: string }) {
  const when = p.status === "published" ? fmt.ago(p.scheduleAt) : fmt.time(p.scheduleAt);
  const line = (p.caption.text || "").split("\n")[0];
  return (
    <Link href={href} className="card tcard sc-pcard block" title={p.caption.text || undefined}>
      <div className="sc-prow">
        <div className="sc-pthumb">
          <Media asset={p.image.asset} />
        </div>
        <div className={`sc-cap ${p.caption.text ? "" : "sc-empty"}`}>
          {line || (p.lease ? "being drafted…" : "no caption yet")}
        </div>
      </div>
      <div className="sc-meta">
        <StatusChip s={p.status} />
        {p.lease && <span className="chip bg-info/10 text-info">✍</span>}
        <span className="sc-dests">
          {p.destinations.map((d) => (
            <DestIcon key={d} d={d} />
          ))}
        </span>
        <span className="sc-when">{when}</span>
      </div>
    </Link>
  );
}

/* IG-style preview card */
export function IgPreview({ p }: { p: Post }) {
  const c = api.clients.list().find((c) => c.id === p.client)!;
  return (
    <div className="sc-ig">
      <div className="sc-igh">
        <span className="sc-avatar" style={{ background: c.color }}>{c.initials}</span>
        <div>
          <div className="sc-nm">{c.handle}</div>
          <div className="sc-loc">Hong Kong</div>
        </div>
        <span className="sc-dots">•••</span>
      </div>
      <div className="sc-igimg">
        <Media asset={p.image.asset} />
      </div>
      <div className="sc-igact">
        {ICONS.heart}
        {ICONS.comment}
        {ICONS.send}
        <span style={{ flex: 1 }} />
        {ICONS.bookmark}
      </div>
      <div className="sc-iglikes">128 likes</div>
      <div className="sc-igcap">
        <span className="sc-nm">{c.handle}  </span>
        {p.caption.text || "—"}
      </div>
      <div className="sc-igtime num">
        scheduled {fmt.time(p.scheduleAt)} · {p.destinations.join(" + ")}
      </div>
    </div>
  );
}

/* revision history rail for one post */
export function History({ p }: { p: Post }) {
  const all = [
    ...p.caption.revs.map((r) => ({ ...r, field: "caption", text: r.text })),
    ...p.image.revs.map((r) => ({ ...r, field: "image", text: r.asset })),
  ].sort((a, b) => b.rev - a.rev || (a.at < b.at ? 1 : -1));
  if (!all.length) return <div className="text-label text-ink-500">nothing written yet</div>;
  return (
    <div>
      {all.map((r, i) => {
        const approved = !!p.approval && p.approval.rev === r.rev && r.field === "caption";
        const voided = approved && !!p.approval!.voidedBy;
        return (
          <div className="sc-hrow" key={`${r.field}-${r.rev}-${i}`}>
            <div className="sc-rev num">r{r.rev}</div>
            <div className="sc-bd">
              <div className="sc-ln">
                <span className={`chip ${CHIP_BASE}`}>{r.field}</span>
                <ByChip by={r.by} via={r.via} />
                {r.job && <span className="kicker">{r.job}</span>}
                <span className="kicker">{fmt.ago(r.at)}</span>
              </div>
              <div className="sc-snip" title={r.text}>{r.text}</div>
              {approved && (
                <div className="sc-ln">
                  <span className={`sc-apv ${voided ? "sc-apv-void" : "sc-apv-ok"}`}>
                    approved r{r.rev}{voided ? " ✕" : " ✓"}
                  </span>
                  {voided && <span className="sc-voided">voided by r{p.approval!.voidedBy}</span>}
                </div>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

/* word-diff box for the writer's ask revision */
export function DiffBox({ from, to }: { from: string; to: string }) {
  const toks = diff(from, to);
  return (
    <div className="sc-diffbox">
      {toks.map(([op, tok], i) =>
        op === "=" ? (
          <span key={i}>{tok}</span>
        ) : op === "+" ? (
          <ins key={i}>{tok}</ins>
        ) : (
          <del key={i}>{tok}</del>
        ),
      )}
    </div>
  );
}

/* suggestion card (proposal on a human-owned field) */
export function SuggCard({ s, postHref }: { s: Suggestion; postHref: (id: string) => string }) {
  return (
    <div className="card sc-sugg p-3 flex flex-col gap-2" data-sugg={s.id}>
      <div className="sc-who">
        <ByChip by={s.by} />
        <span className="text-label text-ink-300">suggests on {s.post} · {s.field}</span>
        <span className="kicker ml-auto">{fmt.ago(s.at)}</span>
      </div>
      <div className="sc-prop">{s.text}</div>
      <div className="text-label text-ink-500">{s.note}</div>
      <div className="flex items-center gap-2">
        <button className={BTN_PRIMARY_SM} onClick={() => api.suggestions.accept(s.id)}>Accept</button>
        <button className={BTN_SM} onClick={() => api.suggestions.reject(s.id)}>Reject</button>
        <Link href={postHref(s.post)} className="lnk text-label">open post</Link>
      </div>
    </div>
  );
}

/** Section/page head, in the board's idiom. */
export function PageHead({ title, note, children }: { title: string; note?: string; children?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1.5 mb-4">
      <h1 className="text-section font-semibold text-ink-100 leading-tight">{title}</h1>
      {note && <span className="kicker">{note}</span>}
      {children}
    </div>
  );
}
