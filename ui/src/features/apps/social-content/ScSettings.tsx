/* ScSettings — client, protected terms, disclaimer, timezone, drafting
 * limit, default destinations. Edits are in-memory via api.settings.* and
 * feed back into the validators live. */
import { useState } from "react";
import { api } from "./mock/api";
import { PageHead } from "./widgets";

export default function ScSettings() {
  const c = api.clients.current();
  const s = api.settings.get();
  const [term, setTerm] = useState("");

  const addTerm = () => {
    const t = term.trim();
    if (!t) return;
    api.settings.addTerm(t);
    setTerm("");
  };

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead title="Settings" />
      <div className="grid gap-3 lg:grid-cols-2 max-w-4xl">
        <div className="card p-3.5 flex flex-col gap-2.5">
          <span className="slabel">client</span>
          <div className="sc-kv">
            <span className="sc-k">name</span>
            <span className="text-label text-ink-200">{c.name}</span>
            <span className="sc-k">handle</span>
            <span className="num text-label text-ink-200">@{c.handle}</span>
            <span className="sc-k">connectors</span>
            <span className="text-label text-ink-200">{c.connectors.join(", ")}</span>
            <span className="sc-k">sources</span>
            <span className="num text-label text-ink-200">{api.sources.list().length} posts in library</span>
            <span className="sc-k">posts</span>
            <span className="num text-label text-ink-200">{api.posts.list().length}</span>
          </div>
          <div className="text-label text-ink-500">
            Switch client from the header — every screen re-scopes.
          </div>
        </div>

        <div className="card p-3.5 flex flex-col gap-2.5">
          <span className="slabel">protected terms</span>
          <div className="text-label text-ink-500">
            Every term found in a source post must survive into the caption verbatim — the check on
            each post enforces it.
          </div>
          <div className="sc-terms">
            {s.protected_terms.map((t) => (
              <span key={t} className="sc-term">
                {t}
                <button title="remove" onClick={() => api.settings.removeTerm(t)}>×</button>
              </span>
            ))}
          </div>
          <input
            className="field"
            placeholder="add a term — kept verbatim"
            value={term}
            onChange={(e) => setTerm(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") addTerm();
            }}
            onBlur={addTerm}
          />
        </div>

        <div className="card p-3.5 flex flex-col gap-2.5">
          <span className="slabel">drafting policy</span>
          <div className="sc-kv">
            <span className="sc-k">disclaimer</span>
            <input
              className="field"
              defaultValue={s.disclaimer}
              placeholder="e.g. All prices in HKD"
              key={c.id + s.disclaimer}
              onBlur={(e) => api.settings.update({ disclaimer: e.target.value })}
            />
            <span className="sc-k">timezone</span>
            <select
              className="field"
              defaultValue={s.timezone}
              key={c.id + s.timezone}
              onChange={(e) => api.settings.update({ timezone: e.target.value })}
            >
              {["Asia/Hong_Kong", "Asia/Singapore", "Europe/London"].map((z) => (
                <option key={z} value={z}>{z}</option>
              ))}
            </select>
            <span className="sc-k">draft limit</span>
            <input
              className="field"
              type="number"
              min={1}
              max={50}
              defaultValue={s.drafting_limit}
              key={c.id + s.drafting_limit}
              onChange={(e) => api.settings.update({ drafting_limit: Number(e.target.value) })}
            />
            <span className="sc-k">destinations</span>
            <span className="text-label text-ink-200">{s.destinations.join(", ")}</span>
          </div>
          <div className="text-label text-ink-500">
            A disclaimer set here becomes a validator on every caption. Drafting runs under a
            standing approval: no sends, ≤ limit/day.
          </div>
        </div>

        <div className="card p-3.5 flex flex-col gap-2.5">
          <span className="slabel">prototype</span>
          <div className="text-label text-ink-500">
            All data is in-memory mock behind <span className="num">features/apps/social-content/mock/api.ts</span>.
            Wire-up swaps only that facade — views never see the store.
          </div>
          <div className="text-label text-ink-500">
            Freeze ambient job motion: open with <span className="num">?freeze</span>
          </div>
        </div>
      </div>
    </main>
  );
}
