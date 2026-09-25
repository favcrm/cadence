/* module: ui/views/decide.js — Needs you: the send digest (hold-back ticks,
 * press-and-hold approve), verify mismatches, voided approvals. §5.3/§5.4. */
(function (SC) {
  "use strict";
  const { h, fmt, mediaEl, icons } = SC.dom;

  SC.views = SC.views || {};
  SC.views.needs = function (el) {
    const api = SC.api, w = SC.w;
    const items = api.needsYou.list();

    const sections = [];
    for (const it of items) {
      if (it.kind === "digest") sections.push(digestCard(it.digest));
      if (it.kind === "verify") sections.push(verifyCard(it.post));
      if (it.kind === "voided") sections.push(voidedCard(it.post));
    }

    el.append(
      h("div", { class: "pagehead" },
        h("h1", null, "Needs you"),
        h("span", { class: "note" }, "outward acts wait here — approval pins the exact revision; a later edit voids it")),
      sections.length
        ? h("div", { class: "stack" }, sections)
        : h("div", { class: "card", style: "padding:26px;text-align:center" },
            h("div", { class: "muted" }, "Nothing pending — sends are approved in digests, receipts verify themselves.")));
  };

  function digestCard(d) {
    const api = SC.api;
    const rows = d.items.map((it) => {
      const p = api.posts.get(it.post);
      const voided = !!it.voided;
      return h("div", { class: "ditem" + (it.hold || voided ? " held" : "") },
        h("button", {
          class: "tick" + (!it.hold && !voided ? " on" : ""),
          disabled: voided,
          title: it.hold ? "held back — tick to include" : "untick to hold back",
          onclick: () => api.digest.setHold(d.id, it.post, !it.hold),
        }, icons.check),
        h("div", { class: "thumb" }, mediaEl(p.image.asset)),
        h("div", null,
          h("div", { class: "cap" }, p.caption.text),
          h("div", { class: "sub" },
            `${it.post} · pinned r${it.pinnedRev} · ${fmt.day(p.scheduleAt)} ${fmt.time(p.scheduleAt).split(" ").pop()} · ${p.destinations.join("+")}`,
            voided ? ` — voided by r${it.voided}, returns to next digest` : "")),
        h("div", { class: "acts row" },
          voided ? h("span", { class: "chip fail" }, "voided") : null,
          h("button", { class: "lnk small", onclick: () => (location.hash = "#/post/" + it.post) }, "open")));
    });
    const live = d.items.filter((i) => !i.hold && !i.voided).length;

    // press-and-hold ~700ms: sends are the outward act
    let timer = null, fill = null, btn = null;
    const fillEl = h("span", { class: "fill" });
    const start = () => {
      let n = 0;
      timer = setInterval(() => {
        n += 7; fillEl.style.width = Math.min(n, 100) + "%";
        if (n >= 100) { clearInterval(timer); api.digest.approve(d.id); SC.w.toast(`digest ${d.id} approved — ${live} post(s) scheduled, receipts will verify`, true); }
      }, 50);
    };
    const stop = () => { if (timer && fillEl.style.width !== "100%") { clearInterval(timer); fillEl.style.width = "0%"; } };
    btn = h("button", { class: "btn primary holdbtn", disabled: !live,
      onmousedown: start, onmouseup: stop, onmouseleave: stop,
      ontouchstart: (e) => { e.preventDefault(); start(); }, ontouchend: stop },
      fillEl, h("span", { class: "lb" }, `Hold to approve & schedule ${live}`));

    return h("div", { class: "card digest" },
      h("div", { class: "row" },
        h("span", { class: "slabel" }, `digest ${d.id}`),
        h("span", { class: "chip" }, d.run), h("span", { class: "kicker" }, fmt.ago(d.at)),
        h("span", { class: "grow" }), btn),
      h("div", { class: "small muted" },
        "Ticked posts go out at their scheduled time. Untick to hold one back. Approving pins each post's current revision — a later edit voids it."),
      ...rows);
  }

  function verifyCard(p) {
    const api = SC.api;
    const r = p.receipts[0];
    const approved = { images: 2, captionHash: "31aa" }; // what approved r3 pinned
    return h("div", { class: "card nyitem" },
      h("div", { class: "nyh" },
        h("span", { class: "chip fail" }, "verify mismatch"),
        h("strong", { style: "color:var(--color-ink-100)" }, `${p.id} — what went out doesn't match what you approved`),
        h("span", { class: "kicker" }, fmt.ago(r.publishedAt))),
      h("div", { class: "nyb" },
        `The receipt for ${r.platform} (${r.platformId}) doesn't match approved r${p.approval.rev}. ` +
        `Verify compares what went out with the pinned revision — this is the “carousel published only the first image” class of bug.`),
      h("div", { class: "cmp" },
        h("div", { class: "side" },
          h("span", { class: "slabel" }, `approved r${p.approval.rev}`),
          `${approved.images} images · caption hash ${approved.captionHash}`),
        h("div", { class: "side bad" },
          h("span", { class: "slabel" }, `receipt ${r.platformId}`),
          `${r.images} image · caption hash ${r.captionHash}`)),
      h("div", { class: "row" },
        h("button", { class: "btn sm primary", onclick: () => { api.needsYou.resolveVerify(p.id, "review"); SC.w.toast(`${p.id} sent back to in review`); } }, "Send back to review"),
        h("button", { class: "btn sm", onclick: () => api.needsYou.resolveVerify(p.id, "accept") }, "Accept what went out"),
        h("button", { class: "lnk small", onclick: () => (location.hash = "#/post/" + p.id) }, "open post")));
  }

  function voidedCard(p) {
    const api = SC.api;
    return h("div", { class: "card nyitem info" },
      h("div", { class: "nyh" },
        h("span", { class: "chip info" }, "approval voided"),
        h("strong", { style: "color:var(--color-ink-100)" }, `${p.id} — r${p.approval.voidedBy} edited after the approval`),
        h("span", { class: "kicker" }, fmt.ago(p.approvalVoidAt))),
      h("div", { class: "nyb" },
        `Approvals pin a revision. r${p.approval.voidedBy} changed the caption after r${p.approval.rev} was approved, ` +
        `so the pin no longer matches — the post went back to in review and returns in the next digest.`),
      h("div", { class: "row" },
        h("button", { class: "btn sm", onclick: () => (location.hash = "#/post/" + p.id) }, "open post"),
        h("button", { class: "btn sm lnk", onclick: () => api.needsYou.dismissVoided(p.id) }, "dismiss")));
  }
})(globalThis.SC = globalThis.SC || {});
