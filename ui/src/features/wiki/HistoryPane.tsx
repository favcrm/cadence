import { useEffect, useState } from "react";
import type { Route } from "../../lib/router";
import Link from "../../ui/Link";
import { wiki } from "./api";
import { versionRows, versionWho, type WikiVersion } from "./history";
import { diffLabel, unifiedDiffLines } from "./diff";
import { DiffLines } from "./EditorPane";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { Failure, Loading, Note, WikiToolbar } from "./shared";

/**
 * A page's history (CAD-581): the revision list, the diff between the
 * selected revision and the current one. Restore is not offered: no board
 * serves a restore route.
 */

export default function HistoryPane({
  path,
  navHref,
}: {
  path: string;
  navHref: (route: Route) => string;
}) {
  const t = useTranslate();
  const [entries, setEntries] = useState<WikiVersion[] | null>(null);
  const [currentRev, setCurrentRev] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [canCompare, setCanCompare] = useState(false);
  const [canOpen, setCanOpen] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setEntries(null);
    setError(null);
    setDiff(null);
    wiki
      .history(path)
      .then((resp) => {
        if (cancelled) return;
        const list = resp.entries ?? resp.history ?? [];
        setCanCompare(resp.canCompare !== false);
        setCanOpen(resp.canOpen !== false);
        setEntries(list);
        const current = resp.rev ?? list[0]?.rev ?? null;
        setCurrentRev(current);
        setSelected(resp.canCompare === false ? null : list.find((entry) => entry.rev !== current)?.rev ?? null);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  useEffect(() => {
    if (!selected || !currentRev || selected === currentRev) {
      setDiff(null);
      return;
    }
    let cancelled = false;
    setDiff(null);
    wiki
      .history(path, selected, currentRev)
      .then((resp) => {
        if (!cancelled) setDiff(resp.diff ?? "");
      })
      .catch((e) => {
        if (!cancelled) setDiff(`could not load the diff — ${e instanceof Error ? e.message : String(e)}`);
      });
    return () => {
      cancelled = true;
    };
  }, [path, selected, currentRev]);

  const rows = versionRows(entries ?? [], currentRev);

  return (
    <>
      <WikiToolbar path={path} navHref={navHref} label={t("History")} actions={<>
          <Button href={navHref({ screen: "wiki", mode: "browse", path, query: null })}>{t("Back to page")}</Button>
      </>} />

      {error !== null ? (
        <Failure what="could not load the history" error={error} />
      ) : entries === null ? (
        <Loading what="loading the history…" />
      ) : rows.length === 0 ? (
        <Note>{t("no history yet — the first save commits one revision.")}</Note>
      ) : (
        <div className={canCompare ? "wk-hsplit" : "wk-history-log"}>
          <div className="wk-ver">
            {rows.map((row) => canCompare ? (
              <div key={row.rev}>
              <button
                type="button"
                className={`wk-vrow${selected === row.rev ? " on" : ""}`}
                onClick={() => setSelected(row.rev)}
              >
                <span className="wk-vwho">{versionWho(row)}</span>
                <span className="wk-vwhen num">
                  {row.rev.slice(0, 7)} · {row.when}
                </span>
                {row.summary && <span className="wk-vsum">{row.summary}</span>}
              </button>
              <Link href={navHref({ screen: "wiki", mode: "browse", path, query: null, at: row.rev })}>{t("Open this revision")}</Link>
              </div>
            ) : <div key={row.rev} className="wk-vrow">
              <span className="wk-vwho">{versionWho(row)}</span>
              <span className="wk-vwhen">{row.when}</span>
              {row.summary && <span className="wk-vsum">{row.summary}</span>}
              {(canOpen || row.rev === currentRev) && <Link href={navHref({ screen: "wiki", mode: "browse", path, query: null, at: row.rev })}>{t("Open this revision")}</Link>}
            </div>)}
          </div>
          {canCompare && <div className="min-w-0">
            <div className="slabel wk-difflabel">
              {selected && currentRev ? diffLabel(selected, currentRev) : t("pick a revision to diff")}
            </div>
            {diff === null ? (
              <Note>{t("select a revision on the left to see what changed since.")}</Note>
            ) : diff === "" ? (
              <Note>{t("no textual difference between these revisions.")}</Note>
            ) : (
              <DiffLines lines={unifiedDiffLines(diff)} />
            )}
          </div>}
        </div>
      )}
    </>
  );
}
