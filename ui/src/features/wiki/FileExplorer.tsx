import { useId, useState } from "react";
import Link from "../../ui/Link";
import { IconCaret, IconTrash } from "../../ui/icons";
import { useTranslate } from "../../lib/locale";
import { KindIcon, LockIcon } from "./shared";
import type { TreeRow } from "./tree";

/** Shared explorer: native links and buttons on desktop, collapsible on mobile.
 *  CAD-2018: a proven operator also gets the Trash row below the tree — the
 *  one discovery entry for the deleted-landing screen. A non-operator never
 *  receives it. */
export default function FileExplorer({ label, rows, current, folder, emptyText, hrefFor, onToggle, trash }: {
  label: string;
  rows: TreeRow[];
  current: string;
  folder?: string;
  emptyText: string;
  hrefFor: (path: string) => string;
  onToggle: (path: string) => void;
  /** The operator's Trash screen: where it is, and whether it is open. */
  trash?: { href: string; active: boolean };
}) {
  const t = useTranslate();
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
            <Link className="wk-tname" title={row.name} href={hrefFor(row.path)} aria-current={row.path === current ? "page" : undefined} onClick={() => setShown(false)}>
              {row.name}{row.dir ? "/" : ""}
            </Link>
            {row.own && <span className="kicker">you</span>}
            {row.locked && <LockIcon />}
          </div>
        ))}
        {trash && (
          <div className={`wk-trow wk-trash-row${trash.active ? " on" : ""}`} style={{ paddingLeft: 8 }}>
            <span className="wk-caret-space" />
            <span className="wk-tkind"><IconTrash /></span>
            <Link className="wk-tname" href={trash.href} aria-current={trash.active ? "page" : undefined} onClick={() => setShown(false)}>
              {t("Trash")}
            </Link>
          </div>
        )}
      </nav>
    </aside>
  );
}
