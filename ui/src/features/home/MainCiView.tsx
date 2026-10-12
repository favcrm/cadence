import type { MainCi } from "../../lib/types";
import { shaCiLabel } from "../../lib/uxCopy";
import { useTranslate } from "../../lib/locale";

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

/** The CI status line. `shaCiLabel` (uxCopy) returns English; its fixed words are
 *  translated here and the SHA, conclusion and run text stay raw. */
function shaLabel(s: Parameters<typeof shaCiLabel>[0], t: (text: string) => string): string {
  const raw = shaCiLabel(s);
  const cover = s.covered_by ? `${t("covered by")} ${s.covered_by.slice(0, 7)}` : t("not yet covered");
  if (raw === "passed" || raw === "pending" || raw === "unknown") return t(raw);
  if (raw === "failed") return t("failed");
  if (raw.startsWith("failed (")) return `${t("failed")} (${raw.slice("failed (".length)}`;
  if (raw.startsWith("no ci run")) return `${t("no ci run")} — ${cover}`;
  // cancelled: "<conclusion or cancelled> — <cover>"
  const conclusion = s.conclusion && s.conclusion !== "cancelled" ? s.conclusion : t("cancelled");
  return `${conclusion} — ${cover}`;
}

/** SHAs shown per repo; the rest are counted. */
const MAIN_CI_SHOWN = 8;

/// Default-branch CI per repo, newest SHA first — each SHA judged only
/// by its own `ci.yml` push run (CAD-267).
export default function MainCiView({ blocks }: { blocks: MainCi[] }) {
  const t = useTranslate();
  return (
    <section>
      <div className="slabel mb-2">{t("default-branch ci")}</div>
      <div className="card divide-y divide-ink-700/60">
        {blocks.map((b) => {
          const shas = b.shas ?? [];
          return (
            <div key={b.slug} className="px-4 py-3 space-y-1.5">
              <div className="flex flex-wrap items-baseline gap-x-2 text-micro">
                <span className="num text-label text-ink-200">{b.slug}</span>
                {b.branch && <span className="text-ink-400">{b.branch}</span>}
                <span className="text-ink-600">ci · {b.workflow ?? "ci.yml"} {t("push runs")}</span>
                {b.order === "runs" && (
                  <span className="text-ink-500" title={b.log_error ?? undefined}>
                    {t("ordered by run time — no local first-parent log")}
                  </span>
                )}
              </div>
              {b.error ? (
                <div className="text-label text-warn">
                  {t("cannot read ci runs —")} {b.error}
                </div>
              ) : shas.length === 0 ? (
                <div className="text-label text-ink-500">{t("no default-branch SHAs to classify")}</div>
              ) : (
                <ul className="space-y-0.5">
                  {shas.slice(0, MAIN_CI_SHOWN).map((s) => (
                    <li key={s.sha} className="flex flex-wrap items-center gap-x-2 text-micro">
                      <code className="num text-ink-400 w-16 shrink-0">{s.sha.slice(0, 7)}</code>
                      <span className={`chip !py-[.15rem] ${shaCiChip(s.state, !!s.covered_by)}`}>
                        {s.run_url ? (
                          <a href={s.run_url} target="_blank" rel="noreferrer" className="hover:underline">
                            {shaLabel(s, t)}
                          </a>
                        ) : (
                          t(shaCiLabel(s))
                        )}
                      </span>
                    </li>
                  ))}
                  {shas.length > MAIN_CI_SHOWN && (
                    <li className="text-micro text-ink-600">
                      {shas.length - MAIN_CI_SHOWN} {t("older SHA")}{shas.length - MAIN_CI_SHOWN === 1 ? "" : "s"} {t("not shown")}
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
