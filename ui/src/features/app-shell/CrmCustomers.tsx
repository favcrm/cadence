import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import CrmCampaigns from "./CrmCampaigns";
import type { CrmSection } from "./CrmOutlet";
import CrmSegments from "./CrmSegments";
import {
  buildCustomerProfile,
  consentEntries,
  EMPTY_CUSTOMER_FORM,
  formFromProfile,
  friendlyError,
  isConsentChoice,
  newCustomerId,
  revisionEntries,
  viewProfile,
  type CustomerFormFields,
} from "./customerProfile";
import { hostActions, type HostRecord, type HostScope } from "./hostActions";

/**
 * The CRM workspace inside the trusted shared App shell (CAD-781
 * Customers, CAD-784 Segments and Campaigns). Every section renders
 * server rows only, with a separate New page and direct detail
 * routes — no sample rows anywhere. The outlet remounts on every
 * install/context switch (the shell keys it on scope), so no search
 * text, page cursor or unsaved draft survives the boundary. The
 * nested section itself lives in the route (`crm=`), so direct links
 * and browser back keep it; switching sections clears the record
 * view in the shell. Search text lives in component state only — the
 * route URL keeps scope (`ctx`, `record`) and never record content.
 */
export default function CrmShell({
  scope,
  viewer,
  view,
  recordId,
  section,
  onView,
  onSelect,
  onSection,
  onRecordCreated,
}: {
  scope: HostScope;
  viewer: Viewer;
  view: "list" | "new";
  recordId: string | null;
  section: CrmSection;
  onView: (view: "list" | "new") => void;
  onSelect: (recordId: string | null) => void;
  onSection: (section: CrmSection) => void;
  onRecordCreated?: (recordId: string) => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  return (
    <div className="app-outlet crm" data-outlet="crm">
      <div className="app-outlet-tabs" role="tablist" aria-label="CRM sections">
        {(
          [
            ["customers", "Customers"],
            ["segments", "Segments"],
            ["campaigns", "Campaigns"],
          ] as [CrmSection, string][]
        ).map(([key, label]) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={section === key}
            className="app-outlet-tab"
            data-on={section === key || undefined}
            onClick={() => onSection(key)}
          >
            {label}
          </button>
        ))}
      </div>

      {section === "segments" && (
        <CrmSegments
          scope={scope}
          viewer={viewer}
          view={view}
          recordId={recordId}
          onView={onView}
          onSelect={onSelect}
          onRecordCreated={onRecordCreated}
        />
      )}
      {section === "campaigns" && (
        <CrmCampaigns
          scope={scope}
          viewer={viewer}
          view={view}
          recordId={recordId}
          onView={onView}
          onSelect={onSelect}
          onRecordCreated={onRecordCreated}
        />
      )}

      {section === "customers" && (
        <nav className="crm-crumb" aria-label="Breadcrumb">
          <Link href="/apps" className="lnk text-label">
            Apps
          </Link>
          <span aria-hidden="true" className="text-ink-600">
            /
          </span>
          <span className="text-label text-ink-300">CRM</span>
          <span aria-hidden="true" className="text-ink-600">
            /
          </span>
          <span className="text-label text-ink-100" aria-current="page">
            Customers
            {view === "new" ? " / New" : ""}
            {recordId !== null ? " / Details" : ""}
          </span>
        </nav>
      )}
      {section === "customers" && view === "list" && (
        <CustomerList
          scope={scope}
          viewer={viewer}
          onSelect={onSelect}
          onNew={() => onView("new")}
        />
      )}
      {section === "customers" && view === "new" && (
        <CustomerNew
          scope={scope}
          viewer={viewer}
          onCreated={(id) => {
            // Prefer the shell's atomic landing; the two-step fallback
            // is only for hosts without it (and keeps stale `appview=new`).
            if (onRecordCreated) onRecordCreated(id);
            else {
              onView("list");
              onSelect(id);
            }
          }}
          onCancel={() => onView("list")}
        />
      )}
      {section === "customers" && recordId !== null && (
        <CustomerDrawer
          scope={scope}
          recordId={recordId}
          viewer={viewer}
          canWrite={canWrite}
          onClose={() => onSelect(null)}
        />
      )}
    </div>
  );
}

const PAGE_SIZE = 20;

function CustomerList({
  scope,
  viewer,
  onSelect,
  onNew,
}: {
  scope: HostScope;
  viewer: Viewer;
  onSelect: (recordId: string) => void;
  onNew: () => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const [query, setQuery] = useState("");
  const [committed, setCommitted] = useState("");
  const [cursors, setCursors] = useState<string[]>([]);
  const [retry, setRetry] = useState(0);
  const [records, setRecords] = useState<HostRecord[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const cursor = cursors.length > 0 ? cursors[cursors.length - 1] : undefined;

  // Debounced search: typing never navigates, so no customer content
  // reaches the URL, history or session storage.
  useEffect(() => {
    const timer = setTimeout(() => setCommitted(query.trim()), 250);
    return () => clearTimeout(timer);
  }, [query]);

  // A new search restarts paging from the first page.
  useEffect(() => {
    setCursors([]);
  }, [committed, scope.installId, scope.contextId]);

  const reloadToken = `${scope.installId}:${scope.contextId}:${committed}:${cursor ?? ""}:${retry}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    hostActions
      .list(scope, {
        query: committed === "" ? undefined : committed,
        limit: PAGE_SIZE,
        cursor,
      })
      .then((page) => {
        if (controller.signal.aborted) return;
        setRecords(page.records);
        setTruncated(page.truncated);
        setNextCursor(page.nextCursor);
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setError(friendlyError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // Scope identity plus paging position drive refetch; `scope` itself
    // is a fresh object every render, so depend on its fields.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  return (
    <section aria-label="Customers list" className="crm-list">
      <div className="crm-toolbar">
        <div className="crm-search">
          <label className="sr-only" htmlFor="crm-customer-search">
            Search customers
          </label>
          <input
            id="crm-customer-search"
            className="field crm-search-input"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            maxLength={120}
            autoComplete="off"
            placeholder="Search name, email or tag…"
          />
        </div>
        {canWrite && (
          <Button variant="primary" size="sm" onClick={onNew}>
            New customer
          </Button>
        )}
      </div>
      {!viewer.operator && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Sign in as the operator to inspect customer records.
        </p>
      )}
      {viewer.operator && viewer.readOnly && (
        <p className="card px-4 py-3 text-label text-ink-400" data-state="read-only">
          Read-only view. Record creation and edits are unavailable.
        </p>
      )}
      {scope.contextId === "" && viewer.operator && (
        <p className="card px-4 py-3 text-label text-ink-400">
          Pick an App context above to list its customers.
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && loading && (
        <p className="text-secondary text-ink-400" role="status">
          Reading customers…
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && error !== null && !loading && (
        <p className="card px-4 py-3 text-label text-fail border-fail/40" role="alert">
          {error}{" "}
          <button
            type="button"
            className="lnk"
            onClick={() => setRetry((count) => count + 1)}
          >
            Retry
          </button>
        </p>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && !loading && records.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400" data-empty="customers" role="status">
          <p className="font-medium text-ink-200">
            {committed === "" ? "No customers yet in this context" : "No customers match this search"}
          </p>
          <p className="mt-1">
            {committed === ""
              ? "Create the first record with New customer, or import a CSV once that flow lands. Only real server rows appear here."
              : "Clear the search to see every record in this context."}
          </p>
        </div>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && records.length > 0 && (
        <>
          <div
            className="crm-table-wrap"
            tabIndex={0}
            role="region"
            aria-label="Customers table — scroll horizontally to reach every column"
          >
            <table className="crm-table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Email</th>
                  <th scope="col">Tags</th>
                  <th scope="col">Consent</th>
                  <th scope="col">Rev</th>
                  <th scope="col">
                    <span className="sr-only">Open</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {records.map((record) => {
                  const view = viewProfile(record.profile);
                  return (
                    <tr key={record.id}>
                      <td className="text-ink-100">{view.displayName}</td>
                      <td className="num text-ink-300">{view.email ?? "—"}</td>
                      <td className="text-ink-300">
                        {view.tags.length > 0 ? view.tags.join(", ") : "—"}
                        {view.source ? <span className="num text-micro text-ink-500"> · {view.source}</span> : null}
                      </td>
                      <td>
                        <span className="chip" title="Email consent">
                          {view.consentEmail}
                        </span>
                      </td>
                      <td className="num text-ink-500">r{record.revision}</td>
                      <td>
                        <button type="button" className="lnk" onClick={() => onSelect(record.id)}>
                          Open
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div className="crm-pager">
            <Button
              size="sm"
              disabled={cursors.length === 0 || loading}
              onClick={() => setCursors((prev) => prev.slice(0, -1))}
            >
              ← Previous
            </Button>
            <span className="num text-micro text-ink-500" aria-live="polite">
              {records.length} row{records.length === 1 ? "" : "s"}
              {truncated ? " · more on the server" : ""}
            </span>
            <Button
              size="sm"
              disabled={!truncated || nextCursor === null || loading}
              onClick={() => nextCursor !== null && setCursors((prev) => [...prev, nextCursor])}
            >
              Next →
            </Button>
          </div>
        </>
      )}
    </section>
  );
}

const CONSENT_OPTIONS: { value: string; label: string }[] = [
  { value: "unknown", label: "Unknown — no marketing until granted" },
  { value: "granted", label: "Granted" },
  { value: "denied", label: "Denied" },
];

function CustomerForm({
  initial,
  submitLabel,
  pending,
  formError,
  onSubmit,
}: {
  initial: CustomerFormFields;
  submitLabel: string;
  pending: boolean;
  formError: string | null;
  onSubmit: (fields: CustomerFormFields) => void;
}) {
  const [fields, setFields] = useState<CustomerFormFields>(initial);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const set = (patch: Partial<CustomerFormFields>) => {
    setFields((prev) => ({ ...prev, ...patch }));
    setFieldError(null);
  };
  return (
    <form
      className="card px-4 py-4 grid gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        try {
          buildCustomerProfile(fields);
        } catch (e: unknown) {
          setFieldError(e instanceof ApiError ? e.message : String(e));
          return;
        }
        onSubmit(fields);
      }}
    >
      <div className="crm-field">
        <label className="text-label text-ink-300" htmlFor="crm-display-name">
          Display name (required)
        </label>
        <input
          id="crm-display-name"
          className="field"
          value={fields.displayName}
          onChange={(e) => set({ displayName: e.target.value })}
          maxLength={120}
          autoComplete="off"
          required
        />
      </div>
      <div className="crm-field-row">
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-email">
            Email (optional)
          </label>
          <input
            id="crm-email"
            className="field"
            type="email"
            value={fields.email}
            onChange={(e) => set({ email: e.target.value })}
            maxLength={254}
            autoComplete="off"
            placeholder="name@example.com"
          />
        </div>
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-phone">
            Phone (optional)
          </label>
          <input
            id="crm-phone"
            className="field"
            type="tel"
            value={fields.phone}
            onChange={(e) => set({ phone: e.target.value })}
            maxLength={24}
            autoComplete="off"
          />
        </div>
      </div>
      <div className="crm-field-row">
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-tags">
            Tags (optional, comma separated)
          </label>
          <input
            id="crm-tags"
            className="field"
            value={fields.tags}
            onChange={(e) => set({ tags: e.target.value })}
            maxLength={400}
            autoComplete="off"
            placeholder="vip, newsletter"
          />
        </div>
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-source">
            Source (optional)
          </label>
          <input
            id="crm-source"
            className="field"
            value={fields.source}
            onChange={(e) => set({ source: e.target.value })}
            maxLength={40}
            autoComplete="off"
            placeholder="import"
          />
        </div>
      </div>
      <div className="crm-field-row">
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-consent-email">
            Email consent (explicit)
          </label>
          <Select
            id="crm-consent-email"
            value={fields.consentEmail}
            onChange={(value) => isConsentChoice(value) && set({ consentEmail: value })}
            options={CONSENT_OPTIONS}
            aria-label="Email consent"
            full
          />
        </div>
        <div className="crm-field">
          <label className="text-label text-ink-300" htmlFor="crm-consent-sms">
            SMS consent (explicit)
          </label>
          <Select
            id="crm-consent-sms"
            value={fields.consentSms}
            onChange={(value) => isConsentChoice(value) && set({ consentSms: value })}
            options={CONSENT_OPTIONS}
            aria-label="SMS consent"
            full
          />
        </div>
      </div>
      {(fieldError ?? formError) && (
        <p className="text-label text-fail" role="alert">
          {fieldError ?? formError}
        </p>
      )}
      <div>
        <Button type="submit" variant="primary" loading={pending} disabled={pending}>
          {submitLabel}
        </Button>
      </div>
    </form>
  );
}

function CustomerNew({
  scope,
  viewer,
  onCreated,
  onCancel,
}: {
  scope: HostScope;
  viewer: Viewer;
  onCreated: (recordId: string) => void;
  onCancel: () => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  // Keyboard users land on the heading when the route changes.
  useEffect(() => {
    headRef.current?.focus();
  }, []);
  return (
    <section aria-label="New customer">
      <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1} data-outlet-heading>
        New customer
      </h3>
      <p className="text-label text-ink-400 mt-1">
        <button type="button" className="lnk" onClick={onCancel}>
          ← Customers
        </button>{" "}
        · Context {scope.contextId || "none"} — duplicates and stale writes are refused by the server.
      </p>
      {!canWrite ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400" data-state="read-only">
          Read-only view. A verified operator creates customer records.
        </p>
      ) : scope.contextId === "" ? (
        <p className="card px-4 py-3 mt-2 text-label text-ink-400">
          Pick an App context above before creating a customer.
        </p>
      ) : (
        <div className="mt-2">
          <CustomerForm
            initial={EMPTY_CUSTOMER_FORM}
            submitLabel="Create customer"
            pending={pending}
            formError={formError}
            onSubmit={(fields) => {
              setPending(true);
              setFormError(null);
              let profile: Record<string, unknown>;
              try {
                profile = buildCustomerProfile(fields);
              } catch (e: unknown) {
                setPending(false);
                setFormError(friendlyError(e));
                return;
              }
              void hostActions
                .create(scope, newCustomerId(), profile)
                .then((record) => onCreated(record.id))
                .catch((e: unknown) => setFormError(friendlyError(e)))
                .finally(() => setPending(false));
            }}
          />
        </div>
      )}
    </section>
  );
}

function CustomerDrawer({
  scope,
  recordId,
  viewer,
  canWrite,
  onClose,
}: {
  scope: HostScope;
  recordId: string;
  viewer: Viewer;
  canWrite: boolean;
  onClose: () => void;
}) {
  const [record, setRecord] = useState<HostRecord | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const opener = useRef<Element | null>(null);

  useEffect(() => {
    // Focus restoration: remember the opener, land on the drawer
    // heading, and return focus when the drawer unmounts.
    opener.current = document.activeElement;
    headRef.current?.focus();
    return () => {
      if (opener.current instanceof HTMLElement) opener.current.focus();
    };
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [onClose]);

  const reloadToken = `${scope.installId}:${scope.contextId}:${recordId}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setEditing(false);
    hostActions
      .show(scope, recordId)
      .then((next) => {
        if (!controller.signal.aborted) setRecord(next);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(friendlyError(e));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadToken]);

  const view = record ? viewProfile(record.profile) : null;
  const revisions = record ? revisionEntries(record) : [];
  const consentTrail = record ? consentEntries(record) : [];

  return (
    <div
      className="crm-drawer"
      role="dialog"
      aria-modal="false"
      aria-label="Customer details"
      data-drawer="customer"
    >
      <div className="crm-drawer-head">
        <h3 ref={headRef} className="text-cardtitle font-medium text-ink-100" tabIndex={-1}>
          {loading ? "Customer details" : (view?.displayName ?? "Customer details")}
        </h3>
        <Button size="sm" onClick={onClose} aria-label="Close customer details">
          Close
        </Button>
      </div>
      <p className="num text-micro text-ink-500">
        {recordId} · {scope.contextId || "no context"} · selection is chat context only — the server
        re-proves scope on every send.
      </p>
      {loading && (
        <p className="text-secondary text-ink-400 mt-2" role="status">
          Reading the record…
        </p>
      )}
      {error !== null && !loading && (
        <p className="card px-4 py-3 mt-2 text-label text-fail border-fail/40" role="alert">
          {error}{" "}
          <button
            type="button"
            className="lnk"
            onClick={() => {
              setError(null);
              setLoading(true);
              hostActions
                .show(scope, recordId)
                .then(setRecord)
                .catch((e: unknown) => setError(friendlyError(e)))
                .finally(() => setLoading(false));
            }}
          >
            Retry
          </button>
        </p>
      )}
      {!loading && error === null && record !== null && view !== null && (
        <>
          <dl className="crm-detail mt-2">
            <div>
              <dt>Email</dt>
              <dd className="num">{view.email ?? "—"}</dd>
            </div>
            <div>
              <dt>Phone</dt>
              <dd className="num">{view.phone ?? "—"}</dd>
            </div>
            <div>
              <dt>Tags</dt>
              <dd>{view.tags.length > 0 ? view.tags.join(", ") : "—"}</dd>
            </div>
            <div>
              <dt>Source</dt>
              <dd>{view.source ?? "—"}</dd>
            </div>
            <div>
              <dt>Email consent</dt>
              <dd>
                <span className="chip">{view.consentEmail}</span>
              </dd>
            </div>
            <div>
              <dt>SMS consent</dt>
              <dd>
                <span className="chip">{view.consentSms ?? "unknown"}</span>
              </dd>
            </div>
            <div>
              <dt>Revision</dt>
              <dd className="num">
                r{record.revision} · <span title="Server content digest">{record.digest.slice(0, 18)}…</span>
              </dd>
            </div>
          </dl>
          {consentTrail.length > 0 && (
            <section aria-label="Consent history" className="mt-3">
              <h4 className="text-label font-medium text-ink-200">Consent history</h4>
              <ol className="crm-history">
                {consentTrail.map((entry) => (
                  <li key={`${entry.revision}-${entry.channel}`} className="num text-label text-ink-300">
                    r{entry.revision} · {entry.channel}: {entry.state} · {entry.actor} ·{" "}
                    {new Date(entry.at * 1000).toISOString().slice(0, 10)}
                  </li>
                ))}
              </ol>
            </section>
          )}
          {revisions.length > 0 && (
            <section aria-label="Revision history" className="mt-3">
              <h4 className="text-label font-medium text-ink-200">Revision history</h4>
              <ol className="crm-history">
                {revisions.map((entry) => (
                  <li key={entry.revision} className="num text-label text-ink-300">
                    r{entry.revision} · {entry.actor} · {entry.digest.slice(0, 18)}… ·{" "}
                    {new Date(entry.at * 1000).toISOString().slice(0, 10)}
                  </li>
                ))}
              </ol>
            </section>
          )}
          {canWrite ? (
            editing ? (
              <div className="mt-3">
                <h4 className="text-label font-medium text-ink-200 mb-2">
                  Edit — expected revision r{record.revision}
                </h4>
                <CustomerForm
                  key={record.revision}
                  initial={formFromProfile(record.profile)}
                  submitLabel={`Save as r${record.revision + 1}`}
                  pending={pending}
                  formError={formError}
                  onSubmit={(fields) => {
                    setPending(true);
                    setFormError(null);
                    let profile: Record<string, unknown>;
                    try {
                      profile = buildCustomerProfile(fields);
                    } catch (e: unknown) {
                      setPending(false);
                      setFormError(friendlyError(e));
                      return;
                    }
                    void hostActions
                      .update(scope, recordId, record.revision, profile)
                      .then((next) => {
                        setRecord(next);
                        setEditing(false);
                      })
                      .catch((e: unknown) => setFormError(friendlyError(e)))
                      .finally(() => setPending(false));
                  }}
                />
                <p className="mt-2">
                  <button type="button" className="lnk text-label" onClick={() => setEditing(false)}>
                    Discard edit
                  </button>
                </p>
              </div>
            ) : (
              <p className="mt-3">
                <Button size="sm" onClick={() => { setEditing(true); setFormError(null); }}>
                  Edit profile
                </Button>
              </p>
            )
          ) : (
            <p className="text-label text-ink-400 mt-3" data-state="read-only">
              Read-only view. {viewer.operator ? "Edits are disabled on this board." : "Sign in as the operator to edit."}
            </p>
          )}
        </>
      )}
    </div>
  );
}
