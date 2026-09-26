/* mock/api.ts — the facade every screen calls.
 * Shaped like the future commands API (app.read / app.call): lists, record
 * reads, then verbs — edit, takeOver, ask, accept, approve. When the app gets
 * wired, only this file is replaced; views and mock/data.ts stay.
 *
 * It also simulates agent work: the running job advances on a timer, `ask`
 * resolves after a beat, published receipts verify themselves. `?freeze`
 * stops ambient motion so screenshots and demos are stable — user-triggered
 * calls still resolve.
 */
import { useSyncExternalStore } from "react";
import {
  M,
  NOW,
  type Automation,
  type AutomationCfg,
  type AutomationTrigger,
  type Digest,
  type Post,
  type Run,
} from "./data";

export const FREEZE = /[?&]freeze/.test(location.search + location.hash);
const TICK = 2400;

/* ---------- tiny event bus ---------- */
const subs = new Set<() => void>();
let version = 0;
const emit = () => {
  version += 1;
  subs.forEach((f) => f());
};

let tickTimer: number | undefined;
function startTick(): void {
  if (FREEZE || tickTimer !== undefined) return;
  tickTimer = window.setInterval(tick, TICK);
}
function stopTick(): void {
  if (tickTimer !== undefined) {
    clearInterval(tickTimer);
    tickTimer = undefined;
  }
}

/** The ambient engine runs while at least one view subscribes. */
function subscribe(f: () => void): () => void {
  subs.add(f);
  startTick();
  return () => {
    subs.delete(f);
    if (subs.size === 0) stopTick();
  };
}
const getVersion = () => version;

/** Re-render the component on every mock mutation; returns the version. */
export function useMock(): number {
  return useSyncExternalStore(subscribe, getVersion);
}

/* ---------- helpers ---------- */
const post = (id: string): Post => M.posts.find((p) => p.id === id)!;
const postOrNull = (id: string): Post | undefined => M.posts.find((p) => p.id === id);
const src = (id: string) => M.sourcePosts.find((s) => s.id === id);
const run = (id: string): Run | undefined => M.runs.find((r) => r.id === id);
const cur = () => M.clients.find((c) => c.id === M.currentClient)!;
const hash = (s: string) => {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h.toString(16).slice(-4);
};
const nowIso = () => new Date(NOW.getTime() + M.clock * 60000).toISOString();
const log = (msg: string) => {
  M.activity.unshift([nowIso(), msg]);
};
const runlog = (r: Run, msg: string) => {
  r.log.push([nowIso(), msg]);
};
const short = (t: string, n = 46) => (t.length > n ? t.slice(0, n - 1) + "…" : t);

/* The `every` line on an automation card, derived from its saved trigger. */
const DOW_SHORT = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const TZ_SHORT: Record<string, string> = {
  "Asia/Hong_Kong": "HKT",
  "Asia/Singapore": "SGT",
  "Europe/London": "GMT",
};
export function describeTrigger(t: AutomationTrigger): string {
  if (t.kind === "interval") return `every ${t.hours ?? "?"}h`;
  if (t.kind === "weekly") {
    const ds = [...(t.weekdays ?? [])].sort();
    const days =
      ds.length === 5 && ds.join() === "1,2,3,4,5"
        ? "Mon–Fri"
        : ds.map((d) => DOW_SHORT[d]).join("·") || "weekly";
    const tz = TZ_SHORT[t.timezone ?? ""] ?? (t.timezone ?? "").split("/").pop() ?? "";
    return `${days} ${t.time ?? ""} ${tz}`.trim();
  }
  return t.event === "sources" ? "when new sources arrive" : "after each publish";
}

/* canned writer output for simulated drafts (per source id) */
const DRAFTS: Record<string, string> = {
  s1: "黑蒜豚骨登場——18 小時熬湯,每日限量 40 碗。HK$128。星期四起兩店同步。#KuraHK All prices in HKD",
  s2: "中環店 10 月 1 日私人活動,提早至 21:30 截單。尖沙咀店照常營業至 23:00。",
  s3: "主廚 Mori 親解:為何叉燒要即叫即炙。完整影片在網站。#KuraHK",
  s7: "10 月起球場每日 07:00–23:00 開放,App 內直接訂場。#VelvetPadel",
  s8: "秋季梯隊賽:64 位球手、3 星期賽期,10 月 18 日決賽,歡迎觀戰。#VelvetPadel",
  s9: "新教練 Marta 10 月 1 日加盟 Velvet Padel,逢週二、四開班。#VelvetPadel",
  s10: "會籍由 HK$380/月起:非繁忙時段任用所有球場,附送訪客券。#VelvetPadel",
  s11: "Recovery Zone 開放:桑拿 + 冰浴,會員優先。#VelvetPadel",
  s12: "Velvet x Siux 限量球拍,20 支,會員 48 小時優先。#VelvetPadel",
  s13: "十月 TSUKEMEN 回歸:粗麵條配濃縮沾汁,尖沙咀店限定。#KuraHK",
};
const draftFor = (sid: string) => DRAFTS[sid] || "本地化草稿(待檢閱)。";

/* ---------- validators (the checks the editor shows live) ---------- */
export interface Check {
  label: string;
  ok: boolean;
  det: string;
}
function checksFor(p: Post): Check[] {
  const cli = M.clients.find((c) => c.id === p.client)!;
  const terms = cli.settings.protected_terms;
  const srcPost = src(p.source);
  // Terms that appear in the source must survive into the caption verbatim.
  const needed = terms.filter((t) => srcPost && srcPost.text.includes(t));
  const missing = needed.filter((t) => !p.caption.text.includes(t));
  const disc = cli.settings.disclaimer;
  const rows: Check[] = [
    { label: "keeps protected terms", ok: missing.length === 0,
      det: missing.length ? "missing " + missing.join(", ") : needed.join(", ") || "none in source" },
  ];
  // the disclaimer is required only when the caption quotes a price
  if (disc && /HK\$/i.test(p.caption.text))
    rows.push({ label: "HKD disclaimer on prices", ok: p.caption.text.includes(disc), det: disc });
  rows.push({ label: "≤ 2200 chars", ok: p.caption.text.length <= 2200, det: String(p.caption.text.length) });
  return rows;
}

/* ---------- revision write, the single choke point ---------- */
function writeRev(p: Post, field: "caption", text: string, by: string, via: string | undefined, job: string | null) {
  const f = p[field];
  f.rev += 1;
  f.text = text;
  const r = { rev: f.rev, by, via, job: job || null, at: nowIso(), text };
  f.revs.push(r);
  // Any change after an approval voids it (the pinned revision no longer
  // matches the head) — the post returns to the next digest.
  if (p.approval && !p.approval.voidedBy) {
    p.approval.voidedBy = f.rev;
    p.approvalVoidAt = nowIso();
    const d = M.digests.find((d) => d.id === p.approval!.digest);
    const it = d && d.items.find((i) => i.post === p.id);
    if (it) it.voided = f.rev;
    log(`${p.id} approval (r${p.approval.rev}) voided by r${f.rev}`);
    if (p.status === "waiting" || p.status === "ready") setStatus(p, "in_review", "approval voided — back for the brand check");
  }
  return r;
}
function setStatus(p: Post, s: Post["status"], why?: string) {
  p.status = s;
  if (why) log(`${p.id} → ${s} (${why})`);
}

/* ---------- simulated agent work ---------- */
// user-triggered work resolves on a short real timer (even frozen — a demo
// or screenshot still needs the ask to land); ambient progression uses tick
function after(ticks: number, fn: () => void) {
  window.setTimeout(fn, FREEZE ? 120 : ticks * 1400);
}

/* A drafting run drives adapt → visuals → review. schedule and verify are
 * event-driven instead: the schedule step lands when a digest approves the
 * post, verify when the publisher writes receipts. */
const SEQ = ["adapt", "visuals", "review", "schedule", "verify"];
const DRAFT_SEQ = ["adapt", "visuals", "review"];
const WHO: Record<string, string> = { adapt: "writer", visuals: "designer", review: "editor", schedule: "publisher", verify: "analyst" };

/* Complete the item's running step; when adapt/visuals complete the agent
 * writes its leased field (unless a human already took it over — sticky). */
function finishStep(r: Run, it: Run["items"][number], step: string) {
  it.steps[step] = "done";
  const p = postOrNull(it.post);
  if (!p) return;
  if (step === "adapt") {
    const itemKey = `${r.id}:i${r.items.indexOf(it) + 1}`;
    if (p.lease && p.lease.jobItem === itemKey) p.lease = null;
    // sticky: once a person touched the caption, the writer never writes over it
    if (p.tookOver || p.caption.revs.some((x) => x.by === "you"))
      runlog(r, `${p.id} caption left to you — nothing written`);
    else {
      writeRev(p, "caption", draftFor(p.source), "writer", undefined, r.id);
      runlog(r, `writer wrote ${p.id} caption r${p.caption.rev}`);
    }
  }
  if (step === "visuals" && !p.image.revs.length) {
    const m = src(p.source)?.media[0];
    const asset = m ? m.seed : `poster-${p.id}`;
    p.image.rev = 1;
    p.image.asset = asset;
    p.image.revs.push({ rev: 1, by: "designer", job: r.id, at: nowIso(), asset });
    runlog(r, `designer set ${p.id} image r1 (${m ? "kept source" : "poster render"})`);
  }
  if (step === "review") {
    const failed = checksFor(p).filter((c) => !c.ok);
    if (failed.length) {
      it.steps.review = "failed";
      it.steps.schedule = "skipped";
      it.steps.verify = "skipped";
      setStatus(p, "in_review", "editor check failed — needs a human pass");
      runlog(r, `editor check failed on ${p.id} (${failed.map((c) => c.label).join(", ")}) → stays in review`);
    } else {
      setStatus(p, "ready");
      runlog(r, `editor check passed on ${p.id} → ready, waits for the next digest`);
    }
  }
}

/* Advance an item one step: finish whatever is running, else start the next
 * drafting step. Returns true when something moved. */
function advanceItem(r: Run, it: Run["items"][number]): boolean {
  const running = SEQ.find((s) => it.steps[s] === "running");
  if (running) {
    finishStep(r, it, running);
    return true;
  }
  const nxt = DRAFT_SEQ.find((s) => it.steps[s] === "waiting");
  if (nxt) {
    it.steps[nxt] = "running";
    runlog(r, `${WHO[nxt]} started ${nxt} on ${it.post}`);
    return true;
  }
  return false;
}

function settleRun(r: Run) {
  const resolved = (i: Run["items"][number]) =>
    DRAFT_SEQ.every((s) => i.steps[s] === "done" || i.steps[s] === "failed");
  if (r.items.every(resolved) && r.status === "running") {
    r.status = "done";
    runlog(r, "run complete — ready posts wait for the next digest");
  }
}

function tick() {
  M.clock += 2;
  let moved = false;
  for (const r of M.runs.filter((r) => r.status === "running" && r.kind !== "revise"))
    for (const it of r.items) moved = advanceItem(r, it) || moved;
  for (const r of M.runs.filter((r) => r.status === "running" && r.kind !== "revise")) settleRun(r);
  if (moved) emit();
}

export interface NeedsYouItem {
  kind: "digest" | "verify" | "voided";
  title: string;
  at: string;
  digest?: Digest;
  post?: Post;
}

/* ---------- the facade ---------- */
export const api = {
  NOW: () => new Date(NOW.getTime() + M.clock * 60000),
  freeze: FREEZE,

  clients: {
    list: () => M.clients,
    current: cur,
    switch(id: string) {
      M.currentClient = id;
      M.selection.clear();
      log(`client → ${cur().name}`);
      emit();
    },
  },

  sources: {
    list: () => M.sourcePosts.filter((s) => s.client === M.currentClient && !s.hidden),
    all: () => M.sourcePosts,
    byId: src,
    selectedIds: () => [...M.selection].filter((id) => src(id)?.client === M.currentClient),
    toggle(id: string) {
      if (M.selection.has(id)) M.selection.delete(id);
      else M.selection.add(id);
      emit();
    },
    clear() {
      M.selection.clear();
      emit();
    },
    /** "Skip / hide" — drop a source out of the grid (mock state only). */
    hide(id: string) {
      const s = src(id);
      if (!s) return;
      s.hidden = true;
      M.selection.delete(id);
      log(`${id} hidden by you`);
      emit();
    },
  },

  posts: {
    list: () => M.posts.filter((p) => p.client === M.currentClient),
    all: () => M.posts,
    get: postOrNull,
    /** Drafts/posts made from a source post — the drawer's "made from this". */
    fromSource: (sid: string) => M.posts.filter((p) => p.source === sid),
    sourceOf: (p: Post) => src(p.source),
    checks: (id: string) => checksFor(post(id)),
    // validators against an unsaved draft — the editor shows these live
    checkCaption: (id: string, text: string) => {
      const p = post(id);
      return checksFor({ ...p, caption: { ...p.caption, text } });
    },

    editCaption(id: string, text: string) {
      const p = post(id);
      if (p.lease) {
        const r = run(p.lease.jobItem.split(":")[0]);
        if (r) runlog(r, `${p.id} caption taken over by you`);
        log(`${p.id} lease broken — you took over the caption`);
        p.lease = null;
        p.tookOver = true;
      }
      writeRev(p, "caption", text, "you", undefined, null);
      // human edit on a post an agent passed: recheck only (brand gate)
      if (p.status === "ready") setStatus(p, "in_review", "edited after ready — brand check only");
      emit();
    },
    takeOver(id: string) {
      const p = post(id);
      if (!p.lease) return;
      const r = run(p.lease.jobItem.split(":")[0]);
      if (r) runlog(r, `${p.id} caption taken over by you — job item's caption need cleared`);
      log(`${p.id} caption taken over by you`);
      p.lease = null;
      p.tookOver = true;
      emit();
    },
    ask(id: string, instruction: string) {
      const p = post(id);
      const rid = "run-" + M.nextRun++;
      const r: Run = { id: rid, kind: "revise", client: p.client, status: "running", at: nowIso(),
        label: `Ask: “${short(instruction, 40)}” on ${p.id}`,
        items: [{ post: p.id, steps: { change: "running" } }],
        log: [[nowIso(), `ask by you → revise ${p.id} (scope: caption)`]] };
      M.runs.unshift(r);
      p.undo = { text: p.caption.text, rev: p.caption.rev };
      log(`${p.id} ask queued — “${short(instruction, 60)}”`);
      emit();
      after(1, () => {
        // mock writer: apply the instruction as a visible transform
        let t = p.caption.text;
        const tag = (t.match(/#\S+/) || [cur().settings.protected_terms.find((x) => x.startsWith("#")) || "#post"])[0];
        if (/short/i.test(instruction)) t = t.replace(/\s*#\S+.*$/, "").split("。")[0] + "。" + tag;
        else if (/disclaim|prices|HKD/i.test(instruction) && cur().settings.disclaimer && !t.includes(cur().settings.disclaimer)) t += " " + cur().settings.disclaimer;
        else if (/hashtag|tag/i.test(instruction) && !t.includes(tag)) t += " " + tag;
        else t = t + (t.endsWith("。") ? "" : "。") + "已按你的要求調整。";
        const base = p.undo!.text;
        writeRev(p, "caption", t, "writer", "asked by you", rid);
        if (p.status === "in_review" || p.status === "drafting") setStatus(p, "in_review");
        r.items[0].steps.change = "done";
        r.status = "done";
        runlog(r, `writer wrote r${p.caption.rev} (asked by you) — diff shown`);
        p.diff = { from: base, to: t, rev: p.caption.rev };
        emit();
      });
    },
    undo(id: string) {
      const p = post(id);
      if (!p.undo) return;
      writeRev(p, "caption", p.undo.text, "you", "undo", null);
      p.diff = null;
      p.undo = null;
      emit();
    },
    uploadImage(id: string, dataUrl: string, name?: string) {
      const p = post(id);
      p.image.rev += 1;
      p.image.asset = dataUrl; // content-addressed in the real store
      p.image.uploaded = true;
      p.image.revs.push({ rev: p.image.rev, by: "you", job: null, at: nowIso(), asset: name || "upload" });
      if (p.approval && !p.approval.voidedBy) p.approval.voidedBy = p.image.rev;
      log(`${p.id} image r${p.image.rev} uploaded by you (${name || "file"})`);
      emit();
    },
    newImage(id: string) {
      const p = post(id);
      log(`${p.id} new image asked of designer`);
      emit();
      after(1, () => {
        p.image.rev += 1;
        p.image.asset = "gen-" + p.id + "-" + p.image.rev;
        p.image.generated = true;
        p.image.revs.push({ rev: p.image.rev, by: "designer", via: "asked by you", job: null, at: nowIso(), asset: p.image.asset });
        emit();
      });
    },
    setTime(id: string, iso: string) {
      const p = post(id);
      p.scheduleAt = iso;
      log(`${p.id} rescheduled by you`);
      emit();
    },
    toggleDestination(id: string, d: string) {
      const p = post(id);
      const i = p.destinations.indexOf(d);
      if (i >= 0) p.destinations.splice(i, 1);
      else p.destinations.push(d);
      emit();
    },
    dismissDiff(id: string) {
      const p = post(id);
      p.diff = null;
      emit();
    },
  },

  suggestions: {
    list: () => M.suggestions.filter((s) => !s.done && postOrNull(s.post)?.client === M.currentClient),
    forPost: (id: string) => M.suggestions.filter((s) => s.post === id && !s.done),
    accept(id: string) {
      const s = M.suggestions.find((s) => s.id === id)!;
      const p = post(s.post);
      // accept writes a revision attributed to the person
      if (s.field === "caption") writeRev(p, "caption", s.text, "you", "accepted suggestion " + s.id, null);
      s.done = true;
      log(`${s.id} accepted — wrote ${p.id} r${p.caption.rev} by you`);
      emit();
    },
    reject(id: string) {
      const s = M.suggestions.find((s) => s.id === id)!;
      s.done = true;
      s.rejected = true;
      log(`${s.id} rejected`);
      emit();
    },
  },

  runs: {
    list: () => M.runs.filter(() => true),
    get: run,
    draft(sourceIds: string[]): string {
      const rid = "run-" + M.nextRun++;
      const items: Run["items"] = [];
      sourceIds.forEach((sid, i) => {
        const pid = "p" + M.nextPost++;
        const sp = src(sid)!;
        const p: Post = { id: pid, client: sp.client, source: sid, status: "drafting",
          caption: { rev: 0, text: "", revs: [] }, image: { rev: 0, asset: null, revs: [] },
          scheduleAt: new Date(NOW.getTime() + (48 + i * 18) * 3600e3).toISOString(),
          destinations: [...cur().settings.destinations], lease: null, job: rid, approval: null, receipts: [] };
        M.posts.push(p);
        items.push({ post: pid, steps: { adapt: "waiting", visuals: "waiting", review: "waiting", schedule: "waiting", verify: "waiting" } });
        sp.isNew = false;
      });
      const r: Run = { id: rid, kind: "social-localize", client: M.currentClient, status: "running",
        at: nowIso(), label: `Draft ${sourceIds.length} posts`, items,
        log: [[nowIso(), `run created from Library selection (${sourceIds.length} sources)`]] };
      M.runs.unshift(r);
      M.selection.clear();
      log(`${rid} started — drafting ${sourceIds.length} post(s)`);
      items.forEach((it, i) =>
        after(1 + i, () => {
          if (it.steps.adapt === "waiting") {
            it.steps.adapt = "running";
            runlog(r, `writer started adapt on ${it.post}`);
            emit();
          }
        }),
      );
      // live mode: tick() drives the run; frozen mode has no ticks, so chain
      // real-time advances — a drafted run still completes in a demo.
      if (FREEZE)
        for (const it of items)
          for (let k = 2; k <= 6; k++)
            after(k, () => {
              advanceItem(r, it);
              settleRun(r);
              emit();
            });
      emit();
      return rid;
    },
  },

  digest: {
    current: () => M.digests.find((d) => d.status === "pending" && d.client === M.currentClient) || null,
    list: () => M.digests,
    setHold(did: string, pid: string, hold: boolean) {
      const d = M.digests.find((d) => d.id === did)!;
      d.items.find((i) => i.post === pid)!.hold = hold;
      emit();
    },
    approve(did: string) {
      const d = M.digests.find((d) => d.id === did)!;
      d.status = "approved";
      const went: string[] = [];
      for (const it of d.items) {
        if (it.hold || it.voided) continue;
        const p = post(it.post);
        setStatus(p, "scheduled", `digest ${d.id} approved`);
        const r0 = run(p.job);
        const it0 = r0 && r0.items.find((i) => i.post === p.id);
        if (it0 && it0.steps.schedule === "waiting") it0.steps.schedule = "done";
        went.push(p.id);
        // mock publish+verify a few ticks later
        after(3, () => {
          setStatus(p, "published");
          p.receipts = p.destinations.map((pl) => ({ platform: pl, platformId: "9" + Math.floor(Math.random() * 9e8),
            url: "https://" + pl + ".com/p/mock-" + p.id, publishedAt: nowIso(), verify: "ok",
            images: 1, captionHash: hash(p.caption.text) }));
          const r = run(p.job);
          const it2 = r && r.items.find((i) => i.post === p.id);
          if (it2) {
            it2.steps.verify = "done";
            runlog(r, `${p.id} receipt verified`);
          }
          emit();
        });
      }
      log(`digest ${d.id} approved — ${went.length} post(s) scheduled${d.items.some((i) => i.hold) ? ", held back: " + d.items.filter((i) => i.hold).map((i) => i.post).join(", ") : ""}`);
      emit();
    },
  },

  needsYou: {
    list(): NeedsYouItem[] {
      const out: NeedsYouItem[] = [];
      const d = M.digests.find((d) => d.status === "pending" && d.client === M.currentClient);
      if (d) {
        const n = d.items.filter((i) => !i.voided).length;
        out.push({ kind: "digest", digest: d, title: `Schedule digest ${d.id} — ${n} post${n === 1 ? "" : "s"}`, at: d.at });
      }
      for (const p of M.posts) {
        if (p.client !== M.currentClient) continue;
        if (p.status === "needs_you" && p.receipts.some((r) => r.verify === "mismatch"))
          out.push({ kind: "verify", post: p, title: `Verify mismatch on ${p.id}`, at: p.receipts[0].publishedAt });
        if (p.approval && p.approval.voidedBy && !p.voidSeen)
          out.push({ kind: "voided", post: p, title: `Approval voided on ${p.id}`, at: p.approvalVoidAt ?? "" });
      }
      return out;
    },
    resolveVerify(pid: string, how: "review" | "accept") {
      const p = post(pid);
      if (how === "review") {
        setStatus(p, "in_review", "sent back by you after verify mismatch");
        p.receipts = [];
      } else {
        p.status = "published";
        p.receipts.forEach((r) => (r.verify = "accepted"));
        p.voidSeen = true;
        log(`${pid} mismatch accepted by you`);
      }
      emit();
    },
    dismissVoided(pid: string) {
      const p = post(pid);
      p.voidSeen = true;
      emit();
    },
  },

  automations: {
    list: () => M.automations,
    get: (id: string) => M.automations.find((a) => a.id === id),
    toggle(id: string) {
      const a = M.automations.find((a) => a.id === id)!;
      a.on = !a.on;
      log(`automation ${a.name} ${a.on ? "resumed" : "paused"} by you`);
      emit();
    },
    /** Save edited settings — the `every` label re-derives from trigger. */
    update(id: string, patch: { name?: string; cfg?: AutomationCfg }) {
      const a = M.automations.find((a) => a.id === id);
      if (!a) return;
      if (patch.name !== undefined && patch.name.trim()) a.name = patch.name.trim();
      if (patch.cfg) {
        a.cfg = patch.cfg;
        a.every = describeTrigger(patch.cfg.trigger);
        const wf = M.workflows.find((w) => w.id === patch.cfg!.workflow);
        if (wf) a.desc = wf.note;
      }
      log(`automation ${a.name} saved`);
      emit();
    },
    /** "New automation" — created paused so the first save is reviewable. */
    create(seed: { name: string; cfg: AutomationCfg }): string {
      const id = "a" + M.nextAuto++;
      const wf = M.workflows.find((w) => w.id === seed.cfg.workflow);
      M.automations.push({
        id,
        name: seed.name,
        desc: wf?.note ?? "",
        every: describeTrigger(seed.cfg.trigger),
        on: false,
        last: "never run",
        approval: "sends always wait — digest",
        cfg: seed.cfg,
      });
      log(`automation ${seed.name} created`);
      emit();
      return id;
    },
    remove(id: string) {
      const i = M.automations.findIndex((a) => a.id === id);
      if (i < 0) return;
      const [a] = M.automations.splice(i, 1);
      log(`automation ${a.name} deleted`);
      emit();
    },
    /** Manual fire — mock queues it and stamps the last-run line. */
    runNow(id: string) {
      const a = M.automations.find((a) => a.id === id);
      if (!a) return;
      a.last = "queued just now — watch Runs";
      log(`automation ${a.name} run requested by you`);
      emit();
    },
    /** Last 5 runs whose workflow matches, scoped like the automation. */
    recentRuns: (a: Automation) =>
      M.runs
        .filter(
          (r) =>
            r.kind === a.cfg.workflow &&
            (a.cfg.scopeClients === "all" || r.client === M.currentClient),
        )
        .sort((x, y) => (x.at < y.at ? 1 : -1))
        .slice(0, 5),
  },

  workflows: { list: () => M.workflows },

  settings: {
    get: () => cur().settings,
    addTerm(t: string) {
      const s = cur().settings;
      if (t && !s.protected_terms.includes(t)) s.protected_terms.push(t);
      emit();
    },
    removeTerm(t: string) {
      const s = cur().settings;
      s.protected_terms = s.protected_terms.filter((x) => x !== t);
      emit();
    },
    update(patch: Partial<import("./data").ClientSettings>) {
      Object.assign(cur().settings, patch);
      emit();
    },
  },

  activity: () => M.activity,
};
