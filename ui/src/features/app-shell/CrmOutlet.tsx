import { useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import type { Viewer } from "../projects/work";
import CrmShell from "./CrmCustomers";
import type { HostScope } from "./hostActions";

export type OutletView = "list" | "new";

/** CRM nested sections under Apps → CRM (CAD-784). */
export type CrmSection = "customers" | "segments" | "campaigns";

/**
 * The installed-App outlet (CAD-802 shell, CAD-781 CRM Customers).
 * CRM installations render the real customer list/new/detail drawer;
 * every other App keeps the truthful coming-soon surface with the
 * same list/detail/new shape — no fake records, ever.
 */
export default function CrmOutlet({
  scope,
  installationTitle,
  appKind,
  view,
  recordId,
  section,
  viewer,
  onView,
  onSelect,
  onSection,
  onRecordCreated,
}: {
  scope: HostScope;
  installationTitle: string;
  /** CRM names its records; every other App stays neutral. */
  appKind: "crm" | "generic";
  view: OutletView;
  recordId: string | null;
  /** Nested CRM section from the route; generic Apps ignore it. */
  section?: CrmSection;
  viewer: Viewer;
  onView: (view: OutletView) => void;
  onSelect: (recordId: string | null) => void;
  onSection?: (section: CrmSection) => void;
  /** Atomic created-record landing (list + details in one URL write). */
  onRecordCreated?: (recordId: string) => void;
}) {
  if (appKind === "crm") {
    return (
      <CrmShell
        scope={scope}
        viewer={viewer}
        view={view}
        recordId={recordId}
        section={section ?? "customers"}
        onView={onView}
        onSelect={onSelect}
        onSection={onSection ?? (() => undefined)}
        onRecordCreated={onRecordCreated}
      />
    );
  }
  return (
    <GenericOutlet
      scope={scope}
      installationTitle={installationTitle}
      view={view}
      recordId={recordId}
      viewer={viewer}
      onView={onView}
      onSelect={onSelect}
    />
  );
}

function GenericOutlet({
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
              This App's record screens are not installed yet. This outlet stays empty until the server returns real rows — nothing here is sample data.
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
              Read-only view. A verified operator creates records once this App's screens land.
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
                placeholder="Unsaved draft — this App's form wires this to the server"
              />
              <p className="text-micro text-ink-500">
                Saving is disabled until this App's form lands. This draft never leaves the browser.
              </p>
              <div>
                <Button type="submit" variant="primary" disabled title="Record creation arrives with this App's screens">
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
            Record <span className="num">{recordId}</span> has no reader yet.
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
