import { useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import type { Viewer } from "../projects/work";
import type { HostScope } from "./hostActions";

export type OutletView = "list" | "new";

/**
 * The generic installed-App outlet (CAD-802). CAD-781 fills this frame
 * with the real CRM customer list/new/detail; until then it is a
 * truthful coming-soon surface with the same list/detail/new shape —
 * no fake customer records, ever.
 */
export default function CrmOutlet({
  scope,
  installationTitle,
  view,
  recordId,
  viewer,
  onView,
  onSelect,
}: {
  scope: HostScope;
  installationTitle: string;
  view: OutletView;
  recordId: string | null;
  viewer: Viewer;
  onView: (view: OutletView) => void;
  onSelect: (recordId: string | null) => void;
}) {
  const [draft, setDraft] = useState("");
  const canWrite = viewer.operator && !viewer.readOnly;
  const newHeadRef = useRef<HTMLHeadingElement | null>(null);
  // Keyboard users land on the New heading when the view changes.
  useEffect(() => {
    if (view === "new") newHeadRef.current?.focus();
  }, [view]);
  return (
    <div className="app-outlet" data-outlet="records">
      <div className="app-outlet-tabs" role="tablist" aria-label="Records">
        <button
          type="button"
          role="tab"
          aria-selected={view === "list"}
          className="app-outlet-tab"
          data-on={view === "list" || undefined}
          onClick={() => onView("list")}
        >
          List
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={view === "new"}
          className="app-outlet-tab"
          data-on={view === "new" || undefined}
          onClick={() => onView("new")}
        >
          New
        </button>
      </div>

      {view === "list" && (
        <section aria-label="Records list">
          <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="records" role="status">
            <p className="font-medium text-ink-200">No records yet in {installationTitle}</p>
            <p className="mt-1">
              Customer records arrive with CAD-781. This outlet stays empty until the server returns real rows —
              nothing here is sample data.
            </p>
            <p className="num text-micro text-ink-500 mt-2">
              Installation {scope.installId} · Context {scope.contextId || "none"}
            </p>
          </div>
        </section>
      )}

      {view === "new" && (
        <section aria-label="New record">
          <h3 ref={newHeadRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
            New record
          </h3>
          {!canWrite ? (
            <p className="card px-4 py-3 mt-2 text-label text-ink-400">
              Read-only view. A verified operator creates records once CAD-781 lands.
            </p>
          ) : (
            <form
              className="card px-4 py-4 mt-2 grid gap-2"
              onSubmit={(e) => e.preventDefault()}
            >
              <label className="text-label text-ink-300" htmlFor="app-outlet-draft">
                Draft name (kept locally until you save — switching installation or context discards it)
              </label>
              <input
                id="app-outlet-draft"
                className="wa-input"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                maxLength={120}
                autoComplete="off"
                placeholder="Unsaved draft — CAD-781 wires this to the server"
              />
              <p className="text-micro text-ink-500">
                Saving is disabled until CAD-781 provides the customer form. This draft never leaves the browser.
              </p>
              <div>
                <Button type="submit" variant="primary" disabled title="Record creation arrives with CAD-781">
                  Save (coming soon)
                </Button>
              </div>
            </form>
          )}
        </section>
      )}

      {recordId !== null && (
        <div
          className="app-outlet-drawer"
          role="dialog"
          aria-modal="false"
          aria-label="Record details"
        >
          <div className="app-outlet-drawer-head">
            <h3 className="text-cardtitle font-medium text-ink-100">Record details</h3>
            <Button size="sm" onClick={() => onSelect(null)} aria-label="Close record details">
              Close
            </Button>
          </div>
          <p className="text-secondary text-ink-400">
            Record <span className="num">{recordId}</span> has no reader yet — CAD-781 provides the detail drawer.
            The selection stays in the URL so a refresh or pasted link keeps it.
          </p>
          <p className="mt-2">
            <Link href="/apps" className="lnk text-label">
              ← All apps
            </Link>
          </p>
        </div>
      )}
    </div>
  );
}
