import type { MainCi } from "../../lib/types";
import { shaCiLabel } from "../../lib/uxCopy";

/** A covered cancelled/missing SHA stays neutral — never the pass colour. */
function shaCiChip(state: string, covered: boolean): string {
  switch (state) {
    case "passed":
      return "bg-ok/15 text-ok";
    case "failed":
      return "bg-fail/10 text-fail";
    case "pending":
      return "bg-info/10 text-info";
    default:
      return covered ? "bg-ink-800 text-ink-400" : "bg-warn/10 text-warn";
  }
}

/** SHAs shown per repo; the rest are counted. */
const MAIN_CI_SHOWN = 8;

/// Default-branch CI per repo, newest SHA first — each SHA judged only
/// by its own `ci.yml` push run (CAD-267).
export default function MainCiView({ blocks }: { blocks: MainCi[] }) {
  return (
    <section>
      <div className="slabel mb-2">default-branch ci</div>
      <div className="card divide-y divide-ink-700/60">
        {blocks.map((b) => {
          const shas = b.shas ?? [];
          return (
            <div key={b.slug} className="px-4 py-3 space-y-1.5">
              <div className="flex flex-wrap items-baseline gap-x-2 text-micro">
                <span className="num text-label text-ink-200">{b.slug}</span>
                {b.branch && <span className="text-ink-400">{b.branch}</span>}
                <span className="text-ink-600">ci · {b.workflow ?? "ci.yml"} push runs</span>
                {b.order === "runs" && (
                  <span className="text-ink-500" title={b.log_error ?? undefined}>
                    ordered by run time — no local first-parent log
                  </span>
                )}
              </div>
              {b.error ? (
                <div className="text-label text-warn">
                  cannot read ci runs — {b.error}
                </div>
              ) : shas.length === 0 ? (
                <div className="text-label text-ink-500">no default-branch SHAs to classify</div>
              ) : (
                <ul className="space-y-0.5">
                  {shas.slice(0, MAIN_CI_SHOWN).map((s) => (
                    <li key={s.sha} className="flex flex-wrap items-center gap-x-2 text-micro">
                      <code className="num text-ink-400 w-16 shrink-0">{s.sha.slice(0, 7)}</code>
                      <span className={`chip !py-[.15rem] ${shaCiChip(s.state, !!s.covered_by)}`}>
                        {s.run_url ? (
                          <a href={s.run_url} target="_blank" rel="noreferrer" className="hover:underline">
                            {shaCiLabel(s)}
                          </a>
                        ) : (
                          shaCiLabel(s)
                        )}
                      </span>
                    </li>
                  ))}
                  {shas.length > MAIN_CI_SHOWN && (
                    <li className="text-micro text-ink-600">
                      {shas.length - MAIN_CI_SHOWN} older SHA{shas.length - MAIN_CI_SHOWN === 1 ? "" : "s"} not shown
                    </li>
                  )}
                </ul>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}
