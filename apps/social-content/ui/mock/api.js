/* module: ui/mock/api.js — the facade every screen calls.
 * Shaped like the future commands API (app.read / app.call): lists, record
 * reads, then verbs — edit, takeOver, ask, accept, approve. When the app gets
 * wired, only this file is replaced; views and mock/data.js stay.
 *
 * It also simulates agent work: the running job advances on a timer, `ask`
 * resolves after a beat, published receipts verify themselves. `?freeze`
 * stops ambient motion so screenshots and demos are stable — user-triggered
 * calls still resolve.
 */
(function (SC) {
  "use strict";
  const M = SC.mock;
  const FREEZE = /[?&]freeze/.test(location.search + location.hash);
  const TICK = FREEZE ? 0 : 2400;

  /* ---------- tiny event bus ---------- */
  const subs = new Set();
  const emit = () => subs.forEach((f) => f());
  const on = (f) => (subs.add(f), () => subs.delete(f));

  /* ---------- helpers ---------- */
  const M_D = M;
  const post = (id) => M_D.posts.find((p) => p.id === id);
  const src = (id) => M_D.sourcePosts.find((s) => s.id === id);
  const run = (id) => M_D.runs.find((r) => r.id === id);
  const cur = () => M_D.clients.find((c) => c.id === M_D.currentClient);
  const hash = (s) => { let h = 0; for (const c of s) h = (h * 31 + c.charCodeAt(0)) >>> 0; return h.toString(16).slice(-4); };
  const nowIso = () => new Date(M.NOW.getTime() + M.clock * 60000).toISOString();
  const log = (msg) => { M.activity.unshift([nowIso(), msg]); };
  const runlog = (r, msg) => r.log.push([nowIso(), msg]);
  const short = (t, n = 46) => (t.length > n ? t.slice(0, n - 1) + "…" : t);

  /* canned writer output for simulated drafts (per source id) */
  const DRAFTS = {
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
  const draftFor = (sid) => DRAFTS[sid] || "本地化草稿(待檢閱)。";

  /* ---------- validators (the checks the editor shows live) ---------- */
  function checksFor(p) {
    const cli = M.clients.find((c) => c.id === p.client);
    const terms = cli.settings.protected_terms;
    const srcPost = src(p.source);
    // Terms that appear in the source must survive into the caption verbatim.
    const needed = terms.filter((t) => srcPost && srcPost.text.includes(t));
    const missing = needed.filter((t) => !p.caption.text.includes(t));
    const disc = cli.settings.disclaimer;
    const rows = [
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
  function writeRev(p, field, text, by, via, job) {
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
      const d = M_D.digests.find((d) => d.id === p.approval.digest);
      const it = d && d.items.find((i) => i.post === p.id);
      if (it) it.voided = f.rev;
      log(`${p.id} approval (r${p.approval.rev}) voided by r${f.rev}`);
      if (p.status === "waiting" || p.status === "ready") setStatus(p, "in_review", "approval voided — back for the brand check");
    }
    return r;
  }
  function setStatus(p, s, why) {
    p.status = s;
    if (why) log(`${p.id} → ${s} (${why})`);
  }

  /* ---------- simulated agent work ---------- */
  // user-triggered work resolves on a short real timer (even frozen — a demo
  // or screenshot still needs the ask to land); ambient progression uses tick
  function after(ticks, fn) { setTimeout(fn, FREEZE ? 120 : ticks * 1400); }

  /* A drafting run drives adapt → visuals → review. schedule and verify are
   * event-driven instead: the schedule step lands when a digest approves the
   * post, verify when the publisher writes receipts. */
  const SEQ = ["adapt", "visuals", "review", "schedule", "verify"];
  const DRAFT_SEQ = ["adapt", "visuals", "review"];
  const WHO = { adapt: "writer", visuals: "designer", review: "editor", schedule: "publisher", verify: "analyst" };

  /* Complete the item's running step; when adapt/visuals complete the agent
   * writes its leased field (unless a human already took it over — sticky). */
  function finishStep(r, it, step) {
    it.steps[step] = "done";
    const p = post(it.post);
    if (!p) return;
    if (step === "adapt") {
      const itemKey = `${r.id}:i${r.items.indexOf(it) + 1}`;
      if (p.lease && p.lease.jobItem === itemKey) p.lease = null;
      // sticky: once a person touched the caption, the writer never writes over it
      if (p._tookOver || p.caption.revs.some((x) => x.by === "you"))
        runlog(r, `${p.id} caption left to you — nothing written`);
      else {
        writeRev(p, "caption", draftFor(p.source), "writer", null, r.id);
        runlog(r, `writer wrote ${p.id} caption r${p.caption.rev}`);
      }
    }
    if (step === "visuals" && !p.image.revs.length) {
      const m = src(p.source).media[0];
      const asset = m ? m.seed : `poster-${p.id}`;
      p.image.rev = 1; p.image.asset = asset;
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
  function advanceItem(r, it) {
    const running = SEQ.find((s) => it.steps[s] === "running");
    if (running) { finishStep(r, it, running); return true; }
    const nxt = DRAFT_SEQ.find((s) => it.steps[s] === "waiting");
    if (nxt) { it.steps[nxt] = "running"; runlog(r, `${WHO[nxt]} started ${nxt} on ${it.post}`); return true; }
    return false;
  }

  function settleRun(r) {
    const resolved = (i) => DRAFT_SEQ.every((s) => i.steps[s] === "done" || i.steps[s] === "failed");
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
  if (!FREEZE) setInterval(tick, TICK);

  /* ---------- the facade ---------- */
  SC.api = {
    on, NOW: () => new Date(M.NOW.getTime() + M.clock * 60000),
    freeze: FREEZE,

    clients: {
      list: () => M.clients,
      current: () => cur(),
      switch(id) { M.currentClient = id; M.selection.clear(); log(`client → ${cur().name}`); emit(); },
    },

    sources: {
      list: () => M.sourcePosts.filter((s) => s.client === M.currentClient),
      all: () => M.sourcePosts,
      selectedIds: () => [...M.selection].filter((id) => src(id).client === M.currentClient),
      toggle(id) { M.selection.has(id) ? M.selection.delete(id) : M.selection.add(id); emit(); },
      clear() { M.selection.clear(); emit(); },
    },

    posts: {
      list: () => M.posts.filter((p) => p.client === M.currentClient),
      all: () => M.posts,
      get: post,
      sourceOf: (p) => src(p.source),
      checks: (id) => checksFor(post(id)),
      // validators against an unsaved draft — the editor shows these live
      checkCaption: (id, text) => checksFor({ ...post(id), caption: { ...post(id).caption, text } }),
      imageAsset: (p) => p.image.asset,

      editCaption(id, text) {
        const p = post(id);
        if (p.lease) { const r = run(p.lease.jobItem.split(":")[0]); if (r) runlog(r, `${p.id} caption taken over by you`); log(`${p.id} lease broken — you took over the caption`); p.lease = null; p._tookOver = true; }
        writeRev(p, "caption", text, "you", null, null);
        // human edit on a post an agent passed: recheck only (brand gate)
        if (p.status === "ready") setStatus(p, "in_review", "edited after ready — brand check only");
        emit();
      },
      takeOver(id) {
        const p = post(id);
        if (!p.lease) return;
        const r = run(p.lease.jobItem.split(":")[0]);
        if (r) runlog(r, `${p.id} caption taken over by you — job item's caption need cleared`);
        log(`${p.id} caption taken over by you`);
        p.lease = null; p._tookOver = true; emit();
      },
      ask(id, instruction) {
        const p = post(id);
        const rid = "run-" + M.nextRun++;
        const r = { id: rid, kind: "revise", client: p.client, status: "running", at: nowIso(),
          label: `Ask: “${short(instruction, 40)}” on ${p.id}`,
          items: [{ post: p.id, steps: { change: "running" } }], log: [[nowIso(), `ask by you → revise ${p.id} (scope: caption)`]] };
        M.runs.unshift(r);
        p._undo = { text: p.caption.text, rev: p.caption.rev };
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
          const base = p._undo.text;
          writeRev(p, "caption", t, "writer", "asked by you", rid);
          if (p.status === "in_review" || p.status === "drafting") setStatus(p, "in_review");
          r.items[0].steps.change = "done"; r.status = "done";
          runlog(r, `writer wrote r${p.caption.rev} (asked by you) — diff shown`);
          p._diff = { from: base, to: t, rev: p.caption.rev };
          emit();
        });
      },
      undo(id) {
        const p = post(id);
        if (!p._undo) return;
        writeRev(p, "caption", p._undo.text, "you", "undo", null);
        p._diff = null; p._undo = null;
        emit();
      },
      uploadImage(id, dataUrl, name) {
        const p = post(id);
        p.image.rev += 1;
        p.image.asset = dataUrl;                 // content-addressed in the real store
        p.image.uploaded = true;
        p.image.revs.push({ rev: p.image.rev, by: "you", job: null, at: nowIso(), asset: name || "upload" });
        if (p.approval && !p.approval.voidedBy) { p.approval.voidedBy = p.image.rev; }
        log(`${p.id} image r${p.image.rev} uploaded by you (${name || "file"})`);
        emit();
      },
      newImage(id) {
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
      setTime(id, iso) { const p = post(id); p.scheduleAt = iso; log(`${p.id} rescheduled by you`); emit(); },
      toggleDestination(id, d) {
        const p = post(id);
        const i = p.destinations.indexOf(d);
        i >= 0 ? p.destinations.splice(i, 1) : p.destinations.push(d);
        emit();
      },
      dismissDiff(id) { const p = post(id); p._diff = null; emit(); },
    },

    suggestions: {
      list: () => M.suggestions.filter((s) => s.status !== "resolved" && post(s.post).client === M.currentClient && !s.done),
      forPost: (id) => M.suggestions.filter((s) => s.post === id && !s.done),
      accept(id) {
        const s = M.suggestions.find((s) => s.id === id);
        const p = post(s.post);
        // accept writes a revision attributed to the person
        writeRev(p, s.field, s.text, "you", "accepted suggestion " + s.id, null);
        s.done = true;
        log(`${s.id} accepted — wrote ${p.id} r${p.caption.rev} by you`);
        emit();
      },
      reject(id) {
        const s = M.suggestions.find((s) => s.id === id);
        s.done = true; s.rejected = true;
        log(`${s.id} rejected`);
        emit();
      },
    },

    runs: {
      list: () => M.runs.filter((r) => true),
      get: run,
      draft(sourceIds) {
        const rid = "run-" + M.nextRun++;
        const items = [], newPosts = [];
        sourceIds.forEach((sid, i) => {
          const pid = "p" + M.nextPost++;
          const sp = src(sid);
          const p = { id: pid, client: sp.client, source: sid, status: "drafting",
            caption: { rev: 0, text: "", revs: [] }, image: { rev: 0, asset: null, revs: [] },
            scheduleAt: new Date(M.NOW.getTime() + (48 + i * 18) * 3600e3).toISOString(),
            destinations: [...cur().settings.destinations], lease: null, job: rid, approval: null, receipts: [] };
          M.posts.push(p); newPosts.push(p);
          items.push({ post: pid, steps: { adapt: "waiting", visuals: "waiting", review: "waiting", schedule: "waiting", verify: "waiting" } });
          sp.isNew = false;
        });
        const r = { id: rid, kind: "social-localize", client: M.currentClient, status: "running",
          at: nowIso(), label: `Draft ${sourceIds.length} posts`, items,
          log: [[nowIso(), `run created from Library selection (${sourceIds.length} sources)`]] };
        M.runs.unshift(r);
        M.selection.clear();
        log(`${rid} started — drafting ${sourceIds.length} post(s)`);
        items.forEach((it, i) => after(1 + i, () => {
          if (it.steps.adapt === "waiting") { it.steps.adapt = "running"; runlog(r, `writer started adapt on ${it.post}`); emit(); }
        }));
        // live mode: tick() drives the run; frozen mode has no ticks, so chain
        // real-time advances — a drafted run still completes in a demo.
        if (FREEZE)
          for (const it of items) for (let k = 2; k <= 6; k++) after(k, () => { advanceItem(r, it); settleRun(r); emit(); });
        emit();
        return rid;
      },
    },

    digest: {
      current: () => M.digests.find((d) => d.status === "pending" && d.client === M.currentClient) || null,
      list: () => M.digests,
      setHold(did, pid, hold) {
        const d = M.digests.find((d) => d.id === did);
        d.items.find((i) => i.post === pid).hold = hold; emit();
      },
      approve(did) {
        const d = M.digests.find((d) => d.id === did);
        d.status = "approved";
        const went = [];
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
            if (it2) { it2.steps.verify = "done"; runlog(r, `${p.id} receipt verified`); }
            emit();
          });
        }
        log(`digest ${d.id} approved — ${went.length} post(s) scheduled${d.items.some((i) => i.hold) ? ", held back: " + d.items.filter((i) => i.hold).map((i) => i.post).join(", ") : ""}`);
        emit();
      },
    },

    needsYou: {
      list() {
        const out = [];
        const d = M.digests.find((d) => d.status === "pending" && d.client === M.currentClient);
        if (d) { const n = d.items.filter((i) => !i.voided).length; out.push({ kind: "digest", digest: d, title: `Schedule digest ${d.id} — ${n} post${n === 1 ? "" : "s"}`, at: d.at }); }
        for (const p of M.posts) {
          if (p.client !== M.currentClient) continue;
          if (p.status === "needs_you" && p.receipts.some((r) => r.verify === "mismatch"))
            out.push({ kind: "verify", post: p, title: `Verify mismatch on ${p.id}`, at: p.receipts[0].publishedAt });
          if (p.approval && p.approval.voidedBy && !p._voidSeen)
            out.push({ kind: "voided", post: p, title: `Approval voided on ${p.id}`, at: p.approvalVoidAt });
        }
        return out;
      },
      resolveVerify(pid, how) {
        const p = post(pid);
        if (how === "review") { setStatus(p, "in_review", "sent back by you after verify mismatch"); p.receipts = []; }
        else { p.status = "published"; p.receipts.forEach((r) => (r.verify = "accepted")); p._voidSeen = true; log(`${pid} mismatch accepted by you`); }
        emit();
      },
      dismissVoided(pid) { const p = post(pid); p._voidSeen = true; emit(); },
    },

    automations: {
      list: () => M.automations,
      toggle(id) { const a = M.automations.find((a) => a.id === id); a.on = !a.on; emit(); },
    },

    workflows: { list: () => M.workflows },

    settings: {
      get: () => cur().settings,
      addTerm(t) { const s = cur().settings; if (t && !s.protected_terms.includes(t)) s.protected_terms.push(t); emit(); },
      removeTerm(t) { const s = cur().settings; s.protected_terms = s.protected_terms.filter((x) => x !== t); emit(); },
      update(patch) { Object.assign(cur().settings, patch); emit(); },
    },

    activity: () => M.activity,
  };

  // initial ambient state
  M.currentClient = "kura";
  M.clock = 0;
  M.nextRun = 107;
  M.nextPost = 10;
})(globalThis.SC = globalThis.SC || {});
