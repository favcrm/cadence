/* module: ui/views/editor.js — Post editor: caption field with lease banner,
 * live checks, image upload/new, Ask the agent with diff + Undo, time and
 * destinations; IG-style preview; revision history (design doc §6).
 *
 * Typing is local (drafts[p.id]) so ambient job updates don't steal focus;
 * "Save caption" is the write — revise(caption, expected_rev) in the wired app.
 */
(function (SC) {
  "use strict";
  const { h, fmt, diffEl } = SC.dom;
  const drafts = {};   // post.id → unsaved caption text
  const asks = {};     // post.id → unsaved ask instruction

  function checksEl(p, text) {
    return h("div", { class: "checks" },
      SC.api.posts.checkCaption(p.id, text).map((c) =>
        h("div", { class: "check " + (c.ok ? "ok" : "bad") },
          h("span", { class: "ck" }, c.ok ? "✓" : "✕"),
          h("span", null, c.label, " ", h("span", { class: "det" }, c.det)))));
  }

  SC.views = SC.views || {};
  SC.views.post = function (el, id) {
    const api = SC.api, w = SC.w;
    const p = api.posts.get(id);
    if (!p) { el.append(h("div", { class: "pagehead" }, h("h1", null, "post not found"))); return; }
    const c = api.clients.current();
    const sp = api.posts.sourceOf(p);
    const draft = drafts[p.id] != null ? drafts[p.id] : p.caption.text;
    const dirty = draft !== p.caption.text;
    const leased = !!p.lease;

    /* ---------- left: the fields ---------- */
    const checksHost = h("div", null, checksEl(p, draft));
    const ta = h("textarea", {
      class: "field", rows: 6, dataset: { fid: "caption-" + p.id },
      disabled: leased, value: draft,
      oninput: (e) => { drafts[p.id] = e.target.value; checksHost.replaceChildren(checksEl(p, e.target.value)); saveRow(); },
    });
    const saveBtn = h("button", {
      class: "btn sm primary", disabled: !dirty || leased,
      onclick: () => { api.posts.editCaption(p.id, drafts[p.id]); delete drafts[p.id]; },
    }, "Save caption");
    const resetBtn = h("button", {
      class: "btn sm", disabled: !dirty || leased,
      onclick: () => { delete drafts[p.id]; SC.app.rerender(); },
    }, "Discard");
    const saveRow = () => { const d = (drafts[p.id] ?? p.caption.text) !== p.caption.text; saveBtn.disabled = d ? leased || false : true; resetBtn.disabled = saveBtn.disabled; dirtyTag.style.display = d ? "" : "none"; };
    const dirtyTag = h("span", { class: "chip warn", style: dirty ? "" : "display:none" }, "unsaved");

    const captionBlock = h("section", { class: "edcard card" },
      h("div", { class: "row" },
        h("span", { class: "slabel" }, `caption · r${p.caption.rev}`),
        w.byChip(lastBy(p.caption), lastVia(p.caption)), dirtyTag),
      leased ? h("div", { class: "leasebar" },
        h("span", null, `✍ ${p.lease.by} is drafting… (${p.lease.jobItem})`),
        h("span", { class: "grow" }),
        h("button", { class: "btn sm", onclick: () => { api.posts.takeOver(p.id); w.toast(`You took over the ${p.id} caption — the writer's in-flight write will be refused.`, true); } }, "Take over")) : null,
      ta,
      checksHost,
      h("div", { class: "row" }, saveBtn, resetBtn,
        p._undo ? h("button", { class: "btn sm danger", onclick: () => api.posts.undo(p.id) }, `Undo r${p.caption.rev} (back to r${p._undo.rev})`) : null),
      p._diff ? h("div", null,
        h("div", { class: "row" },
          h("span", { class: "slabel" }, `writer wrote r${p._diff.rev} (asked by you)`),
          h("span", { class: "grow" }),
          h("button", { class: "lnk small", onclick: () => api.posts.dismissDiff(p.id) }, "dismiss")),
        diffEl(p._diff.from, p._diff.to)) : null);

    /* image */
    const file = h("input", { type: "file", accept: "image/jpeg,image/png", style: "display:none",
      onchange: (e) => {
        const f = e.target.files[0];
        if (!f) return;
        const rd = new FileReader();
        rd.onload = () => { api.posts.uploadImage(p.id, rd.result, f.name); w.toast(`Image uploaded — r${p.image.rev} by you`, true); };
        rd.readAsDataURL(f);
      } });
    const imageBlock = h("section", { class: "edcard card" },
      h("div", { class: "row" }, h("span", { class: "slabel" }, `image · r${p.image.rev}`),
        p.image.revs.length ? w.byChip(lastBy(p.image), lastVia(p.image)) : null),
      h("div", { style: "position:relative;aspect-ratio:16/9;border-radius:.4rem;overflow:hidden" }, SC.dom.mediaEl(p.image.asset)),
      h("div", { class: "row" },
        h("button", { class: "btn sm", onclick: () => file.click() }, "Upload"),
        h("button", { class: "btn sm", onclick: () => { api.posts.newImage(p.id); w.toast("designer asked — new image lands as a revision"); } }, "New image"),
        file));

    /* ask the agent */
    const askInput = h("input", { class: "field", placeholder: "make it shorter, more playful…",
      dataset: { fid: "ask-" + p.id }, value: asks[p.id] || "",
      oninput: (e) => (asks[p.id] = e.target.value),
      onkeydown: (e) => { if (e.key === "Enter") doAsk(); } });
    const doAsk = () => {
      const t = (asks[p.id] || "").trim();
      if (!t) return;
      api.posts.ask(p.id, t); delete asks[p.id];
      w.toast("ask sent — writer will write a revision (diff + undo when it lands)");
    };
    const askBlock = h("section", { class: "edcard card" },
      h("span", { class: "slabel" }, "ask the agent"),
      h("div", { class: "askrow" }, askInput,
        h("button", { class: "btn sm primary", onclick: doAsk }, "Ask")),
      h("div", { class: "small muted" }, "writes a revision attributed “writer (asked by you)” — diff + undo shown"));

    /* time + destinations (human-owned fields) */
    const timeInput = h("input", { type: "datetime-local", class: "field",
      value: fmt.inputLocal(p.scheduleAt),
      onchange: (e) => api.posts.setTime(p.id, new Date(e.target.value + ":00+08:00").toISOString()) });
    const allDests = ["instagram", "facebook", "web"];
    const schedBlock = h("section", { class: "edcard card" },
      h("span", { class: "slabel" }, "time & destinations"),
      h("div", { class: "tworow" },
        h("div", null, h("span", { class: "slabel", style: "display:block;margin-bottom:5px" }, "schedule at (HKT)"), timeInput),
        h("div", null, h("span", { class: "slabel", style: "display:block;margin-bottom:5px" }, "destinations"),
          h("div", { class: "destlist" },
            allDests.map((d) => h("button", {
              class: "dest" + (p.destinations.includes(d) ? " on" : ""),
              onclick: () => api.posts.toggleDestination(p.id, d),
            }, p.destinations.includes(d) ? "✓ " : "", d))))),
      h("div", { class: "small muted" }, `defaults from ${c.name} settings — ${c.settings.destinations.join(", ")}`));

    /* this post's pending suggestions */
    const mySuggs = api.suggestions.forPost(p.id);
    const suggBlock = mySuggs.length ? h("section", { class: "edcard card" },
      h("span", { class: "slabel" }, "suggestions"), mySuggs.map(w.suggCard)) : null;

    /* ---------- assemble ---------- */
    el.append(
      h("div", { class: "pagehead" },
        h("button", { class: "lnk small", onclick: () => history.back() }, "← back"),
        h("h1", null, `${p.id}`),
        w.statusChip(p.status),
        sp ? h("span", { class: "chip" }, `from ${sp.platform}`) : null,
        p.approval && !p.approval.voidedBy ? h("span", { class: "chip ok" }, `approved r${p.approval.rev}`) : null,
        p.approval && p.approval.voidedBy ? h("span", { class: "chip fail" }, `approval voided by r${p.approval.voidedBy}`) : null),
      h("div", { class: "editor-grid" },
        h("div", { class: "stack" }, captionBlock, imageBlock, askBlock, schedBlock, suggBlock),
        h("div", null,
          h("span", { class: "slabel", style: "display:block;margin-bottom:8px" }, "preview"),
          w.igPreview(p),
          h("div", { class: "small muted", style: "margin-top:8px" },
            `receipts: ${p.receipts.length ? p.receipts.map((r) => `${r.platform} ${r.verify === "ok" ? "✓" : r.verify}`).join(" · ") : "—"}`)),
        h("div", { class: "histwrap" },
          h("span", { class: "slabel", style: "display:block;margin-bottom:8px" }, "history"),
          h("div", { class: "card", style: "padding:6px 12px" }, w.history(p)))));

    function lastBy(f) { const r = f.revs[f.revs.length - 1]; return r ? r.by : "system"; }
    function lastVia(f) { const r = f.revs[f.revs.length - 1]; return r && r.via; }
  };
})(globalThis.SC = globalThis.SC || {});
