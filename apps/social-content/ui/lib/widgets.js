/* module: ui/lib/widgets.js — shared view pieces used by several screens:
 * status chips, post cards, the IG-style preview, revision history, toasts,
 * suggestion cards. All read through SC.api; none touches SC.mock.
 */
(function (SC) {
  "use strict";
  const { h, fmt, mediaEl } = SC.dom;
  const api = () => SC.api;

  const STATUS = {
    drafting:   { label: "drafting",   color: "var(--color-info)" },
    in_review:  { label: "in review",  color: "var(--color-warn)" },
    ready:      { label: "ready",      color: "var(--color-violet)" },
    waiting:    { label: "waiting",    color: "var(--color-accent)" },
    scheduled:  { label: "scheduled",  color: "var(--color-info)" },
    published:  { label: "published",  color: "var(--color-ok)" },
    needs_you:  { label: "needs you",  color: "var(--color-fail)" },
  };
  const statusDot = (s) => h("span", { class: "dot", style: `background:${(STATUS[s] || {}).color || "var(--color-ink-500)"}` });
  const statusChip = (s) => {
    const map = { drafting: "info", in_review: "warn", ready: "vio", waiting: "acc", scheduled: "info", published: "ok", needs_you: "fail" };
    return h("span", { class: "chip " + (map[s] || "") }, (STATUS[s] || { label: s }).label);
  };

  const byChip = (by, via) => {
    const cls = by === "you" ? "acc" : by === "system" ? "" : "vio";
    return h("span", { class: "chip " + cls }, by === "you" ? "you" : by + (via ? ` (${via})` : ""));
  };

  const platformChip = (pl) => h("span", { class: "chip" }, pl);

  function postCard(p) {
    const when = p.status === "published" ? fmt.ago(p.scheduleAt) : fmt.time(p.scheduleAt);
    return h("div", { class: "card tcard pcard", onclick: () => (location.hash = "#/post/" + p.id) },
      h("div", { class: "cap" + (p.caption.text ? "" : " empty") },
        p.caption.text || (p.lease ? "being drafted…" : "no caption yet")),
      h("div", { class: "meta" },
        statusChip(p.status),
        p.lease ? h("span", { class: "chip info" }, "writer ✍") : null,
        p.approval && !p.approval.voidedBy ? h("span", { class: "chip ok" }, `✓ r${p.approval.rev}`) : null,
        h("span", { class: "when num" }, when)));
  }

  /* IG-style preview card */
  function igPreview(p) {
    const c = api().clients.list().find((c) => c.id === p.client);
    return h("div", { class: "ig" },
      h("div", { class: "igh" },
        h("span", { class: "avatar", style: `background:${c.color}` }, c.initials),
        h("div", null, h("div", { class: "nm" }, c.handle), h("div", { class: "loc" }, "Hong Kong")),
        h("span", { class: "dots" }, "•••")),
      h("div", { class: "igimg" }, mediaEl(p.image.asset)),
      h("div", { class: "igact" }, SC.dom.icons.heart, SC.dom.icons.comment, SC.dom.icons.send,
        h("span", { style: "flex:1" }), SC.dom.icons.bookmark),
      h("div", { class: "iglikes" }, "128 likes"),
      h("div", { class: "igcap" }, h("span", { class: "nm" }, c.handle + "  "), p.caption.text || "—"),
      h("div", { class: "igtime num" }, `scheduled ${fmt.time(p.scheduleAt)} · ${p.destinations.join(" + ")}`));
  }

  /* revision history rail for one post */
  function history(p) {
    const rows = [];
    const all = [
      ...p.caption.revs.map((r) => ({ ...r, field: "caption" })),
      ...p.image.revs.map((r) => ({ ...r, field: "image", text: r.asset })),
    ].sort((a, b) => b.rev - a.rev || (a.at < b.at ? 1 : -1));
    for (const r of all) {
      const approved = p.approval && p.approval.rev === r.rev && r.field === "caption";
      const voided = approved && p.approval.voidedBy;
      rows.push(h("div", { class: "hrow" },
        h("div", { class: "rev num" }, `r${r.rev}`),
        h("div", { class: "bd" },
          h("div", { class: "ln" },
            h("span", { class: "chip" }, r.field), byChip(r.by, r.via),
            r.job ? h("span", { class: "kicker" }, r.job) : null,
            h("span", { class: "kicker" }, fmt.ago(r.at))),
          h("div", { class: "snip" }, r.text),
          approved ? h("div", { class: "ln" },
            h("span", { class: "apv " + (voided ? "void" : "ok") }, `approved r${r.rev}${voided ? " ✕" : " ✓"}`),
            voided ? h("span", { class: "voided" }, `voided by r${p.approval.voidedBy}`) : null) : null)));
    }
    if (!all.length) rows.push(h("div", { class: "muted small" }, "nothing written yet"));
    return h("div", { class: "hist" }, rows);
  }

  /* suggestion card (proposal on a human-owned field) */
  function suggCard(s) {
    const p = api().posts.get(s.post);
    return h("div", { class: "card sugg", dataset: { sugg: s.id } },
      h("div", { class: "who" },
        byChip(s.by), h("span", { class: "small" }, `suggests on ${s.post} · ${s.field}`),
        h("span", { class: "kicker" }, fmt.ago(s.at))),
      h("div", { class: "prop" }, s.text),
      h("div", { class: "small muted" }, s.note),
      h("div", { class: "acts" },
        h("button", { class: "btn sm primary", onclick: () => api().suggestions.accept(s.id) }, "Accept"),
        h("button", { class: "btn sm", onclick: () => api().suggestions.reject(s.id) }, "Reject"),
        h("button", { class: "btn sm lnk", onclick: () => (location.hash = "#/post/" + s.post) }, "open post")));
  }

  /* toasts */
  let toastHost = null;
  function toast(msg, ok) {
    if (!toastHost) { toastHost = h("div", { class: "toasts" }); document.body.append(toastHost); }
    const t = h("div", { class: "toast reveal" + (ok ? " ok" : "") }, msg);
    toastHost.append(t);
    setTimeout(() => t.remove(), 4200);
  }

  function modal(title, body, actions) {
    const scrim = h("div", { class: "scrim", onclick: (e) => e.target === scrim && scrim.remove() });
    scrim.append(h("div", { class: "modal reveal" },
      h("h2", null, title), body,
      h("div", { class: "row", style: "justify-content:flex-end" }, actions)));
    document.body.append(scrim);
    return scrim;
  }

  SC.w = { STATUS, statusDot, statusChip, byChip, platformChip, postCard, igPreview, history, suggCard, toast, modal };
})(globalThis.SC = globalThis.SC || {});
