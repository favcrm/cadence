/* ScAutomations — scheduled/event triggers running under standing
 * approvals (§5.1 row 3). Toggles are in-memory only. */
import { api } from "./mock/api";
import { PageHead } from "./widgets";

export default function ScAutomations() {
  const list = api.automations.list();
  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Automations"
        note="scheduled/event triggers — they propose; sends still wait for the digest"
      />
      <div className="flex flex-col gap-3 max-w-3xl">
        {list.map((a) => (
          <div key={a.id} className="card p-3.5 flex items-start gap-3.5">
            <button
              className={`sc-toggle ${a.on ? "sc-on" : ""} mt-0.5`}
              role="switch"
              aria-checked={a.on}
              aria-label={a.name}
              onClick={() => api.automations.toggle(a.id)}
            />
            <div className="min-w-0 flex-1">
              <div className="text-label font-medium text-ink-100">{a.name}</div>
              <div className="text-label text-ink-400 mt-0.5">{a.desc}</div>
            </div>
            <div className="text-right flex-none">
              <div className="num text-label text-ink-300">{a.every}</div>
              <div className="kicker mt-0.5">last: {a.last}</div>
              <div className="kicker mt-0.5">{a.approval}</div>
            </div>
          </div>
        ))}
      </div>
    </main>
  );
}
