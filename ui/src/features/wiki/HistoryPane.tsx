import { useEffect, useState } from "react";
import type { Route } from "../../lib/router";
import { navigate } from "../../lib/useLocation";
import { wiki } from "./api";
import { versionRows, versionWho, type WikiVersion } from "./history";
import { diffLabel, unifiedDiffLines } from "./diff";
import { DiffLines } from "./EditorPane";
import Button from "../../ui/Button";
import { Crumbs, Failure, Loading, Note } from "./shared";

/**
 * A page's history (CAD-581): the revision list, the diff between the
 * selected revision and the current one, and Restore — which writes the
 * old text back as a new revision (never a rewrite).
 */

export default function HistoryPane({
  path,
  navHref,
  readOnly,
  onToast,
  onChanged,
}: {
  path: string;
  navHref: (route: Route) => string;
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  onChanged: () => void;
}) {
  const [entries, setEntries] = useState<WikiVersion[] | null>(null);
  const [currentRev, setCurrentRev] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
        setEntries(list);
        const current = resp.rev ?? list[0]?.rev ?? null;
        setCurrentRev(current);
        setSelected(list.find((entry) => entry.rev !== current)?.rev ?? null);
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

  const restore = async () => {
    if (!selected) return;
    setBusy(true);
    try {
      await wiki.restore(path, selected);
      onToast("ok", `restored rev ${selected}`);
      onChanged();
      navigate(navHref({ screen: "wiki", mode: "browse", path, query: null }));
    } catch (e) {
      onToast("err", `restore failed — ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const rows = versionRows(entries ?? [], currentRev);

  return (
    <>
      <div className="wk-bar">
        <Crumbs
          path={path}
          hrefFor={(p) => navHref({ screen: "wiki", mode: "browse", path: p || null, query: null })}
        />
        <span className="wk-sep">·</span>
        <span className="wk-editing">history</span>
        <div className="wk-tools">
          <Button href={navHref({ screen: "wiki", mode: "browse", path, query: null })}>Back to page</Button>
          <Button
            variant="primary"
            disabled={readOnly || busy || !selected || selected === currentRev}
            title={readOnly ? "writes are disabled — sign in as the operator" : undefined}
            onClick={() => void restore()}
          >
            Restore this version
          </Button>
        </div>
      </div>

      {readOnly && <Note warn>writes are disabled on this board — restore needs the operator.</Note>}

      {error !== null ? (
        <Failure what="could not load the history" error={error} />
      ) : entries === null ? (
        <Loading what="loading the history…" />
      ) : rows.length === 0 ? (
        <Note>no history yet — the first save commits one revision.</Note>
      ) : (
        <div className="wk-hsplit">
          <div className="wk-ver">
            {rows.map((row) => (
              <button
                key={row.rev}
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
            ))}
          </div>
          <div className="min-w-0">
            <div className="slabel wk-difflabel">
              {selected && currentRev ? diffLabel(selected, currentRev) : "pick a revision to diff"}
            </div>
            {diff === null ? (
              <Note>select a revision on the left to see what changed since.</Note>
            ) : diff === "" ? (
              <Note>no textual difference between these revisions.</Note>
            ) : (
              <DiffLines lines={unifiedDiffLines(diff)} />
            )}
          </div>
        </div>
      )}
    </>
  );
}
