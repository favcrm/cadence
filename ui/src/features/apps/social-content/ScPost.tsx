/* ScPost — the post editor: caption field with lease banner, live checks,
 * image upload/new, Ask the agent with diff + Undo, time and destinations;
 * IG-style preview; revision history (design doc §6).
 *
 * Typing is local state so ambient job updates don't steal focus;
 * "Save caption" is the write — revise(caption, expected_rev) wired.
 */
import { useRef, useState } from "react";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import {
  ByChip, DiffBox, History, IgPreview, Media, StatusChip, SuggCard,
  BTN_SM, BTN_PRIMARY_SM, PageHead,
} from "./widgets";

export default function ScPost({
  id,
  say,
  postHref,
}: {
  id: string;
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  postHref: (id: string) => string;
}) {
  const p = api.posts.get(id);
  const [draft, setDraft] = useState<string | null>(null);
  const [askText, setAskText] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);

  if (!p) {
    return (
      <main className="px-4 lg:px-8 pt-5 pb-9">
        <PageHead title="post not found" />
        <p className="text-body text-ink-400">
          <span className="num">{id}</span> is not a post of this client.
        </p>
      </main>
    );
  }

  const c = api.clients.current();
  const sp = api.posts.sourceOf(p);
  const text = draft ?? p.caption.text;
  const dirty = text !== p.caption.text;
  const leased = !!p.lease;
  const checks = api.posts.checkCaption(p.id, text);
  const mySuggs = api.suggestions.forPost(p.id);
  const lastCap = p.caption.revs[p.caption.revs.length - 1];
  const lastImg = p.image.revs[p.image.revs.length - 1];
  const allDests = ["instagram", "facebook", "web"];

  const doAsk = () => {
    const t = askText.trim();
    if (!t) return;
    api.posts.ask(p.id, t);
    setAskText("");
    say("ok", "ask sent — writer will write a revision (diff + undo when it lands)");
  };

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead title={p.id} note={sp ? `from ${sp.platform}` : undefined}>
        <button className="lnk text-label" onClick={() => history.back()}>
          ← back
        </button>
        <StatusChip s={p.status} />
        {p.approval && !p.approval.voidedBy && (
          <span className="chip bg-ok/15 text-ok">approved r{p.approval.rev}</span>
        )}
        {p.approval?.voidedBy && (
          <span className="chip bg-fail/10 text-fail">approval voided by r{p.approval.voidedBy}</span>
        )}
      </PageHead>

      <div className="grid gap-4 items-start lg:grid-cols-[minmax(0,1fr)_minmax(0,290px)_minmax(0,290px)]">
        <div className="flex flex-col gap-3 min-w-0">
          {/* caption */}
          <section className="card p-3.5 flex flex-col gap-2.5">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="slabel">caption · r{p.caption.rev}</span>
              {lastCap && <ByChip by={lastCap.by} via={lastCap.via} />}
              {dirty && <span className="chip bg-warn/10 text-warn">unsaved</span>}
            </div>
            {leased && (
              <div className="sc-leasebar">
                <span>✍ {p.lease!.by} is drafting… ({p.lease!.jobItem})</span>
                <span className="flex-1" />
                <button
                  className={BTN_SM}
                  onClick={() => {
                    api.posts.takeOver(p.id);
                    say("ok", `You took over the ${p.id} caption — the writer's in-flight write will be refused.`);
                  }}
                >
                  Take over
                </button>
              </div>
            )}
            <textarea
              className="field !h-auto py-2 leading-relaxed resize-y min-h-[7rem]"
              rows={6}
              disabled={leased}
              value={text}
              onChange={(e) => setDraft(e.target.value)}
            />
            <div className="sc-checks">
              {checks.map((ck) => (
                <div key={ck.label} className={`sc-check ${ck.ok ? "sc-ok" : "sc-bad"}`}>
                  <span className="sc-ck">{ck.ok ? "✓" : "✕"}</span>
                  <span>
                    {ck.label} <span className="sc-det">{ck.det}</span>
                  </span>
                </div>
              ))}
            </div>
            <div className="flex items-center gap-2 flex-wrap">
              <button
                className={BTN_PRIMARY_SM}
                disabled={!dirty || leased}
                onClick={() => {
                  api.posts.editCaption(p.id, text);
                  setDraft(null);
                }}
              >
                Save caption
              </button>
              <button className={BTN_SM} disabled={!dirty || leased} onClick={() => setDraft(null)}>
                Discard
              </button>
              {p.undo && (
                <button className={`${BTN_SM} !text-fail !border-fail/50`} onClick={() => api.posts.undo(p.id)}>
                  Undo r{p.caption.rev} (back to r{p.undo.rev})
                </button>
              )}
            </div>
            {p.diff && (
              <div className="flex flex-col gap-1.5">
                <div className="flex items-center gap-2">
                  <span className="slabel">writer wrote r{p.diff.rev} (asked by you)</span>
                  <span className="flex-1" />
                  <button className="lnk text-label" onClick={() => api.posts.dismissDiff(p.id)}>
                    dismiss
                  </button>
                </div>
                <DiffBox from={p.diff.from} to={p.diff.to} />
              </div>
            )}
          </section>

          {/* image */}
          <section className="card p-3.5 flex flex-col gap-2.5">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="slabel">image · r{p.image.rev}</span>
              {lastImg && <ByChip by={lastImg.by} via={lastImg.via} />}
            </div>
            <div className="relative aspect-video rounded overflow-hidden">
              <Media asset={p.image.asset} />
            </div>
            <div className="flex items-center gap-2">
              <button className={BTN_SM} onClick={() => fileRef.current?.click()}>
                Upload
              </button>
              <button
                className={BTN_SM}
                onClick={() => {
                  api.posts.newImage(p.id);
                  say("ok", "designer asked — new image lands as a revision");
                }}
              >
                New image
              </button>
              <input
                ref={fileRef}
                type="file"
                accept="image/jpeg,image/png"
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (!f) return;
                  const rd = new FileReader();
                  rd.onload = () => {
                    api.posts.uploadImage(p.id, String(rd.result), f.name);
                    say("ok", `Image uploaded — by you`);
                  };
                  rd.readAsDataURL(f);
                  e.target.value = "";
                }}
              />
            </div>
          </section>

          {/* ask the agent */}
          <section className="card p-3.5 flex flex-col gap-2.5">
            <span className="slabel">ask the agent</span>
            <div className="flex gap-2">
              <input
                className="field flex-1 min-w-0"
                placeholder="make it shorter, more playful…"
                value={askText}
                onChange={(e) => setAskText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") doAsk();
                }}
              />
              <button className={BTN_PRIMARY_SM} onClick={doAsk}>Ask</button>
            </div>
            <div className="text-label text-ink-500">
              writes a revision attributed "writer (asked by you)" — diff + undo shown
            </div>
          </section>

          {/* schedule + destinations */}
          <section className="card p-3.5 flex flex-col gap-2.5">
            <span className="slabel">time &amp; destinations</span>
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <span className="slabel block mb-1.5">schedule at (HKT)</span>
                <input
                  type="datetime-local"
                  className="field w-full"
                  value={fmt.inputLocal(p.scheduleAt)}
                  onChange={(e) => {
                    if (e.target.value)
                      api.posts.setTime(p.id, new Date(e.target.value + ":00+08:00").toISOString());
                  }}
                />
              </div>
              <div>
                <span className="slabel block mb-1.5">destinations</span>
                <div className="flex gap-1.5 flex-wrap">
                  {allDests.map((d) => (
                    <button
                      key={d}
                      className={`sc-dest ${p.destinations.includes(d) ? "sc-on" : ""}`}
                      onClick={() => api.posts.toggleDestination(p.id, d)}
                    >
                      {p.destinations.includes(d) ? "✓ " : ""}
                      {d}
                    </button>
                  ))}
                </div>
              </div>
            </div>
            <div className="text-label text-ink-500">
              defaults from {c.name} settings — {c.settings.destinations.join(", ")}
            </div>
          </section>

          {/* suggestions on this post */}
          {mySuggs.length > 0 && (
            <section className="card p-3.5 flex flex-col gap-2.5">
              <span className="slabel">suggestions</span>
              {mySuggs.map((s) => (
                <SuggCard key={s.id} s={s} postHref={postHref} />
              ))}
            </section>
          )}
        </div>

        {/* preview */}
        <div className="min-w-0">
          <span className="slabel block mb-2">preview</span>
          <IgPreview p={p} />
          <div className="text-label text-ink-500 mt-2">
            receipts:{" "}
            {p.receipts.length
              ? p.receipts.map((r) => `${r.platform} ${r.verify === "ok" ? "✓" : r.verify}`).join(" · ")
              : "—"}
          </div>
        </div>

        {/* history */}
        <div className="min-w-0">
          <span className="slabel block mb-2">history</span>
          <div className="card px-3 py-1.5">
            <History p={p} />
          </div>
        </div>
      </div>
    </main>
  );
}
