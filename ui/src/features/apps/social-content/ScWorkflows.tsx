/* ScWorkflows — the app's workflow stubs (plan templates), read-only so the
 * shape is reviewable before anything is wired. */
import { api } from "./mock/api";
import { PageHead } from "./widgets";

export default function ScWorkflows() {
  const list = api.workflows.list();
  return (
    <main className="px-4 lg:px-8 pt-4 pb-9">
      <PageHead
        title="Workflows"
        note="plan templates bundled with the app — each run is a job over records"
      />
      <div className="flex flex-col gap-3 max-w-3xl">
        {list.map((f) => (
          <div key={f.id} className="card p-3.5 flex flex-col gap-2">
            <div className="flex items-center gap-2.5 flex-wrap">
              <strong className="text-label font-semibold text-ink-100">{f.title}</strong>
              <span className="chip bg-ink-800 text-ink-400">{f.id}</span>
              <span className="flex-1" />
              <span className="kicker">{f.file}</span>
            </div>
            <div className="flex items-center gap-1.5 flex-wrap">
              {f.steps.flatMap((s, i) => [
                ...(i ? [<span key={`a${i}`} className="text-ink-600 text-label">→</span>] : []),
                <span key={`s${i}`} className="chip bg-ink-800 text-ink-300">{s}</span>,
              ])}
            </div>
            <div className="text-label text-ink-500">{f.note}</div>
          </div>
        ))}
      </div>
    </main>
  );
}
