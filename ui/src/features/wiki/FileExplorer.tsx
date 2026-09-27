import { useId, useState } from "react";
import { fmtBytes } from "../../lib/fmt";
import Link from "../../ui/Link";
import { IconCaret } from "../../ui/icons";
import { KindIcon, LockIcon } from "./shared";
import type { TreeRow } from "./tree";

/** Shared explorer: native links and buttons on desktop, collapsible on mobile. */
export default function FileExplorer({ label, rows, current, folder, emptyText, hrefFor, onToggle }: {
  label: string;
  rows: TreeRow[];
  current: string;
  folder?: string;
  emptyText: string;
  hrefFor: (path: string) => string;
  onToggle: (path: string) => void;
}) {
  const [shown, setShown] = useState(false);
  const filesId = useId();
  return (
    <aside className="wk-tree">
      <div className="slabel wk-treeroot">{label}</div>
      <button type="button" className="wk-tree-toggle" aria-expanded={shown} aria-controls={filesId} onClick={() => setShown((value) => !value)}>
        <KindIcon kind="dir" /> {label} <IconCaret />
      </button>
      <nav id={filesId} aria-label={label} className={`wk-treefiles${shown ? "" : " collapsed"}`}>
        {rows.length === 0 && <div className="wk-treemeta text-label text-ink-500">{emptyText}</div>}
        {rows.map((row) => (
          <div key={row.path}
            className={`wk-trow${row.dir ? " dir" : ""}${row.expanded ? " open" : ""}${row.path === current || row.path === folder ? " on" : ""}`}
            style={{ paddingLeft: 8 + row.depth * 16 }}>
            {row.dir ? (
              <button type="button" className="wk-caret" aria-label={`${row.expanded ? "Collapse" : "Expand"} ${row.name}`} aria-expanded={row.expanded} onClick={() => onToggle(row.path)}>
                <IconCaret />
              </button>
            ) : <span className="wk-caret-space" />}
            <span className="wk-tkind"><KindIcon kind={row.kind} /></span>
            <Link className="wk-tname" href={hrefFor(row.path)} aria-current={row.path === current ? "page" : undefined} onClick={() => setShown(false)}>
              {row.name}{row.dir ? "/" : ""}
            </Link>
            {row.own && <span className="kicker">you</span>}
            {row.locked && <LockIcon />}
            <span className="wk-tmeta num">{row.size != null ? fmtBytes(row.size) : row.childCount ?? ""}</span>
          </div>
        ))}
      </nav>
    </aside>
  );
}
