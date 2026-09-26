/* ScLibrary — source posts grid, multi-select, "Draft N posts". */
import { useState } from "react";
import { api } from "./mock/api";
import { fmt } from "./fmt";
import { Media, PlatformChip, BTN_SM, BTN_PRIMARY_SM, PageHead } from "./widgets";
import { navigate } from "../../../lib/useLocation";

export default function ScLibrary({
  say,
  runsHref,
}: {
  say: (kind: "ok" | "warn" | "err", text: string) => void;
  runsHref: string;
}) {
  const [newOnly, setNewOnly] = useState(true);
  const list = api.sources.list();
  const sel = new Set(api.sources.selectedIds());
  const shown = newOnly ? list.filter((s) => s.isNew) : list;

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Library"
        note={`source posts — ${api.clients.current().name}`}
      />
      <div className="flex items-center gap-2.5 mb-3 flex-wrap">
        <button
          className={newOnly ? BTN_PRIMARY_SM : BTN_SM}
          onClick={() => setNewOnly((v) => !v)}
        >
          new only{newOnly ? " ✓" : ""}
        </button>
        <span className="text-label text-ink-500">
          {sel.size ? `${sel.size} selected` : `${shown.length} of ${list.length} shown`}
        </span>
        <span className="flex-1" />
        {sel.size > 0 && (
          <button className={BTN_SM} onClick={() => api.sources.clear()}>
            clear
          </button>
        )}
        <button
          className={BTN_PRIMARY_SM}
          disabled={!sel.size}
          onClick={() => {
            const ids = api.sources.selectedIds();
            const rid = api.runs.draft(ids);
            say("ok", `${rid} started — drafting ${ids.length} post(s). See Runs.`);
            navigate(runsHref);
          }}
        >
          {sel.size ? `Draft ${sel.size} post${sel.size > 1 ? "s" : ""}` : "Draft posts"}
        </button>
      </div>

      <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(230px, 1fr))" }}>
        {shown.map((s) => (
          <button
            key={s.id}
            className={`card tcard sc-src text-left ${sel.has(s.id) ? "sc-sel" : ""}`}
            onClick={() => api.sources.toggle(s.id)}
            aria-pressed={sel.has(s.id)}
          >
            <div className="sc-thumb">
              <Media asset={s.media[0]?.seed} />
              <span className="sc-pick">{sel.has(s.id) ? "✓" : ""}</span>
            </div>
            <div className="sc-body">
              <div className="sc-txt">{s.text}</div>
              <div className="sc-meta">
                <PlatformChip pl={s.platform} />
                {s.isNew && (
                  <span className="chip bg-accent/10 text-accent">
                    <span className="sc-newdot" />
                    new
                  </span>
                )}
                <span className="sc-ago">{fmt.ago(s.postedAt)}</span>
              </div>
            </div>
          </button>
        ))}
      </div>
    </main>
  );
}
