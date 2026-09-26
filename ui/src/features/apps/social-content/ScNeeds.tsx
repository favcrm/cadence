/* ScNeeds — Needs you: the send digest (hold-back ticks, press-and-hold
 * approve), verify mismatches, voided approvals. §5.3/§5.4 of the design. */
import { useRef, useState } from "react";
import Link from "../../../ui/Link";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { ICONS, Media, PageHead, BTN_SM, BTN_PRIMARY_SM } from "./widgets";
import type { Digest, Post } from "./mock/data";

const HOLD_MS = 700;

/** Press-and-hold approve — sends are the outward act, so a tap can't fire. */
function HoldApprove({ live, onDone }: { live: number; onDone: () => void }) {
  const [pct, setPct] = useState(0);
  const timer = useRef<number | undefined>(undefined);

  const stop = () => {
    if (timer.current !== undefined) {
      clearInterval(timer.current);
      timer.current = undefined;
      setPct(0);
    }
  };
  const start = () => {
    let elapsed = 0;
    timer.current = window.setInterval(() => {
      elapsed += 50;
      setPct(Math.min(100, (elapsed / HOLD_MS) * 100));
      if (elapsed >= HOLD_MS) {
        clearInterval(timer.current);
        timer.current = undefined;
        onDone();
      }
    }, 50);
  };

  return (
    <button
      className={`${BTN_PRIMARY_SM} sc-holdbtn`}
      disabled={!live}
      onMouseDown={start}
      onMouseUp={stop}
      onMouseLeave={stop}
      onTouchStart={(e) => {
        e.preventDefault();
        start();
      }}
      onTouchEnd={stop}
    >
      <span className="sc-fill" style={{ width: `${pct}%` }} />
      <span className="sc-lb">Hold to approve &amp; schedule {live}</span>
    </button>
  );
}

function DigestCard({ d, say, postHref }: { d: Digest; say: (k: "ok" | "warn" | "err", t: string) => void; postHref: (id: string) => string }) {
  const live = d.items.filter((i) => !i.hold && !i.voided).length;
  return (
    <div className="card p-3.5 flex flex-col gap-2.5">
      <div className="flex items-center gap-2.5 flex-wrap">
        <span className="slabel">digest {d.id}</span>
        <span className="chip bg-ink-800 text-ink-400">{d.run}</span>
        <span className="kicker">{fmt.ago(d.at)}</span>
        <span className="flex-1" />
        <HoldApprove
          live={live}
          onDone={() => {
            api.digest.approve(d.id);
            say("ok", `digest ${d.id} approved — ${live} post(s) scheduled, receipts will verify`);
          }}
        />
      </div>
      <div className="text-label text-ink-500">
        Ticked posts go out at their scheduled time. Untick to hold one back. Approving pins each
        post's current revision — a later edit voids it.
      </div>
      {d.items.map((it) => {
        const p = api.posts.get(it.post);
        if (!p) return null;
        const voided = !!it.voided;
        return (
          <div key={it.post} className={`sc-ditem ${it.hold || voided ? "sc-held" : ""}`}>
            <button
              className={`sc-tick ${!it.hold && !voided ? "sc-on" : ""}`}
              disabled={voided}
              title={it.hold ? "held back — tick to include" : "untick to hold back"}
              onClick={() => api.digest.setHold(d.id, it.post, !it.hold)}
            >
              {ICONS.check}
            </button>
            <div className="sc-thumb">
              <Media asset={p.image.asset} />
            </div>
            <div className="min-w-0">
              <div className="sc-cap">{p.caption.text}</div>
              <div className="sc-sub">
                {it.post} · pinned r{it.pinnedRev} · {fmt.day(p.scheduleAt)}{" "}
                {fmt.time(p.scheduleAt).split(" ").pop()} · {p.destinations.join("+")}
                {voided ? ` — voided by r${it.voided}, returns to next digest` : ""}
              </div>
            </div>
            <div className="sc-acts flex items-center gap-2">
              {voided && <span className="chip bg-fail/10 text-fail">voided</span>}
              <Link href={postHref(it.post)} className="lnk text-label">open</Link>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function VerifyCard({ p, say, postHref }: { p: Post; say: (k: "ok" | "warn" | "err", t: string) => void; postHref: (id: string) => string }) {
  const r = p.receipts[0];
  // what approved r3 pinned — the mock's fixed expectation for this demo
  const approved = { images: 2, captionHash: "31aa" };
  return (
    <div className="card sc-nyitem p-3.5 flex flex-col gap-2.5">
      <div className="flex items-center gap-2.5 flex-wrap">
        <span className="chip bg-fail/10 text-fail">verify mismatch</span>
        <strong className="text-label text-ink-100">
          {p.id} — what went out doesn't match what you approved
        </strong>
        <span className="kicker">{fmt.ago(r.publishedAt)}</span>
      </div>
      <div className="text-label text-ink-400">
        The receipt for {r.platform} ({r.platformId}) doesn't match approved r{p.approval!.rev}.
        Verify compares what went out with the pinned revision — this is the "carousel published
        only the first image" class of bug.
      </div>
      <div className="sc-cmp">
        <div className="sc-side">
          <span className="slabel">approved r{p.approval!.rev}</span>
          {approved.images} images · caption hash {approved.captionHash}
        </div>
        <div className="sc-side sc-bad">
          <span className="slabel">receipt {r.platformId}</span>
          {r.images} image · caption hash {r.captionHash}
        </div>
      </div>
      <div className="flex items-center gap-2 flex-wrap">
        <button
          className={BTN_PRIMARY_SM}
          onClick={() => {
            api.needsYou.resolveVerify(p.id, "review");
            say("ok", `${p.id} sent back to in review`);
          }}
        >
          Send back to review
        </button>
        <button className={BTN_SM} onClick={() => api.needsYou.resolveVerify(p.id, "accept")}>
          Accept what went out
        </button>
        <Link href={postHref(p.id)} className="lnk text-label">open post</Link>
      </div>
    </div>
  );
}

function VoidedCard({ p, postHref }: { p: Post; postHref: (id: string) => string }) {
  return (
    <div className="card sc-nyitem sc-nyinfo p-3.5 flex flex-col gap-2.5">
      <div className="flex items-center gap-2.5 flex-wrap">
        <span className="chip bg-info/10 text-info">approval voided</span>
        <strong className="text-label text-ink-100">
          {p.id} — r{p.approval!.voidedBy} edited after the approval
        </strong>
        <span className="kicker">{p.approvalVoidAt ? fmt.ago(p.approvalVoidAt) : ""}</span>
      </div>
      <div className="text-label text-ink-400">
        Approvals pin a revision. r{p.approval!.voidedBy} changed the caption after r
        {p.approval!.rev} was approved, so the pin no longer matches — the post went back to in
        review and returns in the next digest.
      </div>
      <div className="flex items-center gap-2">
        <Link href={postHref(p.id)} className={`${BTN_SM} no-underline`}>open post</Link>
        <button className={BTN_SM} onClick={() => api.needsYou.dismissVoided(p.id)}>dismiss</button>
      </div>
    </div>
  );
}

export default function ScNeeds({
  say,
  postHref,
}: {
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  postHref: (id: string) => string;
}) {
  const items = api.needsYou.list();
  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Needs you"
        note="outward acts wait here — approval pins the exact revision; a later edit voids it"
      />
      {items.length ? (
        <div className="flex flex-col gap-3 max-w-3xl">
          {items.map((it) =>
            it.kind === "digest" ? (
              <DigestCard key={it.digest!.id} d={it.digest!} say={say} postHref={postHref} />
            ) : it.kind === "verify" ? (
              <VerifyCard key={"v" + it.post!.id} p={it.post!} say={say} postHref={postHref} />
            ) : (
              <VoidedCard key={"o" + it.post!.id} p={it.post!} postHref={postHref} />
            ),
          )}
        </div>
      ) : (
        <div className="card p-6 text-center max-w-3xl">
          <div className="text-label text-ink-500">
            Nothing pending — sends are approved in digests, receipts verify themselves.
          </div>
        </div>
      )}
    </main>
  );
}
