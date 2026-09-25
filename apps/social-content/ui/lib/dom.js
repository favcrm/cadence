/* module: ui/lib/dom.js — tiny DOM + formatting helpers.
 * h(tag, props, ...children) builds elements; no framework, no innerHTML for
 * content (mock text is "untrusted" like real source text would be).
 */
(function (SC) {
  "use strict";

  function h(tag, props, ...kids) {
    const el = document.createElement(tag);
    if (props) for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === "class") el.className = v;
      else if (k === "dataset") Object.assign(el.dataset, v);
      else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
      else if (k === "value") el.value = v;
      else if (k === "checked" || k === "disabled" || k === "selected") el[k] = !!v;
      else el.setAttribute(k, v);
    }
    for (const kid of kids.flat(20)) {
      if (kid == null || kid === false) continue;
      el.append(kid.nodeType ? kid : document.createTextNode(kid));
    }
    return el;
  }

  const TZ = "Asia/Hong_Kong"; // the app's display zone — settings.timezone
  const part = (iso, o) => new Intl.DateTimeFormat("en-GB", { timeZone: TZ, ...o }).format(new Date(iso));
  const DOWS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const dowName = (iso) => part(iso, { weekday: "short" });
  const fmt = {
    // "Thu 09:30" / "Fri 25/9" in the app's pretend HKT clock
    time(iso) { return `${dowName(iso)} ${part(iso, { hour: "2-digit", minute: "2-digit", hour12: false })}`; },
    day(iso) { return `${dowName(iso)} ${part(iso, { day: "numeric", month: "numeric" })}`; },
    dkey(iso) { return part(iso, { year: "numeric", month: "2-digit", day: "2-digit" }); }, // "2026/09/25" — sortable key
    inputLocal(iso) { // for <input type="datetime-local"> — HK wall clock
      const d = part(iso, { year: "numeric", month: "2-digit", day: "2-digit" }).split("/").reverse().join("-");
      return d + "T" + part(iso, { hour: "2-digit", minute: "2-digit", hour12: false });
    },
    weekOf(iso) { // Monday-start day keys (HKT) for the calendar
      const [dd, mm, yyyy] = part(iso, { year: "numeric", month: "2-digit", day: "2-digit" }).split("/").map(Number); // DD/MM/YYYY
      const wd = DOWS.indexOf(dowName(iso));
      const monday = new Date(Date.UTC(yyyy, mm - 1, dd - ((wd + 6) % 7)));
      return Array.from({ length: 7 }, (_, i) => new Date(monday.getTime() + i * 86400e3));
    },
    hkDayKey(dateObj) { return new Intl.DateTimeFormat("en-GB", { timeZone: TZ, year: "numeric", month: "2-digit", day: "2-digit" }).format(dateObj); },
    ago(iso) {
      const m = Math.round((SC.api.NOW() - new Date(iso)) / 60000);
      if (m < 1) return "just now";
      if (m < 60) return m + "m ago";
      const h = Math.round(m / 60);
      if (h < 24) return h + "h ago";
      return Math.round(h / 24) + "d ago";
    },
    in_(iso) {
      const m = Math.round((new Date(iso) - SC.api.NOW()) / 60000);
      if (m < 0) return fmt.ago(iso);
      if (m < 60) return "in " + m + "m";
      const h = Math.round(m / 60);
      if (h < 24) return "in " + h + "h";
      return "in " + Math.round(h / 24) + "d";
    },
  };

  /* Word-ish diff for captions: CJK chars are their own tokens, latin words
   * stay whole, whitespace is preserved. Small LCS — captions are short. */
  function tokenize(s) {
    const out = [];
    for (const ch of s) {
      if (/\s/.test(ch)) out.push(ch);
      else if (/[⺀-鿿豈-﫿]/.test(ch)) out.push(ch);
      else if (out.length && /[\w$@#.-]/.test(out[out.length - 1]) && /[\w$@#.-]/.test(ch)) out[out.length - 1] += ch;
      else out.push(ch);
    }
    return out;
  }
  function diff(a, b) {
    const A = tokenize(a), B = tokenize(b);
    const n = A.length, m = B.length;
    const dp = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
    for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--)
      dp[i][j] = A[i] === B[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    const res = [];
    let i = 0, j = 0;
    while (i < n && j < m) {
      if (A[i] === B[j]) { res.push(["=", A[i]]); i++; j++; }
      else if (dp[i + 1][j] >= dp[i][j + 1]) { res.push(["-", A[i]]); i++; }
      else { res.push(["+", B[j]]); j++; }
    }
    while (i < n) res.push(["-", A[i++]]);
    while (j < m) res.push(["+", B[j++]]);
    return res;
  }
  function diffEl(a, b) {
    const box = h("div", { class: "diffbox" });
    for (const [op, tok] of diff(a, b)) {
      if (op === "=") box.append(tok);
      else box.append(h(op === "+" ? "ins" : "del", null, tok));
    }
    return box;
  }

  /* placeholder media — deterministic gradient per seed, or a data: URL for
   * real uploads (FileReader works from file://, so upload is genuinely
   * clickable offline too). */
  const PALETTES = [
    ["#3b2f2f", "#c2603e"], ["#1f3a3d", "#2dd4bf"], ["#2b2450", "#7c5cd6"],
    ["#402626", "#e05d44"], ["#20304a", "#60a5fa"], ["#3a2f1d", "#fbbf24"],
    ["#1d3428", "#4ade80"], ["#331f38", "#f472b6"],
  ];
  function mediaEl(asset, cls) {
    const wrap = h("div", { class: "phimg " + (cls || "") });
    if (!asset) {
      wrap.style.background = "var(--color-ink-800)";
      wrap.append(h("span", { class: "glyph", style: "opacity:.4" }, "no image"));
      return wrap;
    }
    if (asset.startsWith("data:")) { wrap.append(h("img", { src: asset, alt: "" })); return wrap; }
    let n = 0; for (const c of asset) n = (n + c.charCodeAt(0)) % 997;
    const [c1, c2] = PALETTES[n % PALETTES.length];
    wrap.style.background = `linear-gradient(135deg, ${c1}, ${c2})`;
    wrap.append(h("div", { class: "pat" }));
    if (asset.startsWith("gen-") || asset.startsWith("poster-"))
      wrap.append(h("span", { class: "glyph" }, "✦ gen"));
    else wrap.append(h("span", { class: "glyph" }, asset.replace(/-/g, " ")));
    return wrap;
  }

  const I = (d, extra) => {
    const s = `<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" ${extra || ""}>${d}</svg>`;
    const t = document.createElement("span");
    t.innerHTML = s;
    return t.firstChild;
  };
  const icons = {
    home: I('<circle cx="8" cy="8" r="5.5"/><path d="M8 5.5v3l2 1.4"/>'),
    library: I('<rect x="1.5" y="2" width="3.6" height="12" rx="1"/><rect x="6.2" y="2" width="3.6" height="8" rx="1"/><rect x="10.9" y="2" width="3.6" height="10" rx="1"/>'),
    runs: I('<rect x="2" y="3" width="12" height="8.5" rx="1.2"/><path d="M5.5 14h5"/>'),
    needs: I('<path d="M8 2.5 14.5 13h-13z"/><path d="M8 6.5v3M8 11.3v.2"/>'),
    autos: I('<path d="M3 8a5 5 0 0 1 9-3M13 8a5 5 0 0 1-9 3"/><path d="M12 2v3h-3M4 14v-3h3"/>'),
    flows: I('<path d="M2.5 4.5h4l2.5 7h4.5"/><circle cx="2.5" cy="4.5" r="1.3"/><circle cx="13.5" cy="11.5" r="1.3"/>'),
    gear: I('<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1"/>'),
    check: I('<path d="M3 8.5 6.5 12 13 4.5"/>'),
    x: I('<path d="M4 4l8 8M12 4l-8 8"/>'),
    heart: I('<path d="M8 13.5s-5.5-3.4-5.5-7.4A3.1 3.1 0 0 1 8 4.2a3.1 3.1 0 0 1 5.5 1.9c0 4-5.5 7.4-5.5 7.4z"/>', 'fill="none"'),
    comment: I('<path d="M13.5 8a5.5 5.5 0 0 1-9 4.3L2.5 13.5l1.2-2A5.5 5.5 0 1 1 13.5 8z"/>'),
    send: I('<path d="M13.8 2.2 7 9M13.8 2.2l-4 11.6-2.8-4.8-4.8-2.8z"/>'),
    bookmark: I('<path d="M4 2.5h8V14L8 11l-4 3z"/>'),
  };

  SC.dom = { h, fmt, diffEl, mediaEl, icons };
})(globalThis.SC = globalThis.SC || {});
