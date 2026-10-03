import { useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import type { Viewer } from "../projects/work";
import CrmCampaigns from "./CrmCampaigns";
import type { CrmSection } from "./CrmOutlet";
import CrmSegments from "./CrmSegments";
import CustomerCsvImport from "./CustomerCsvImport";
import Detail from "./shared/Detail";
import DataTable from "./shared/DataTable";
import Field from "./shared/Field";
import { EmptyState, ErrorNotice, Loading, Notice } from "./shared/States";
import DrawerShell, { type DrawerTab } from "./shared/DrawerShell";
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
 *
 * The shell's own header row is the single Apps → App breadcrumb and
 * title; each section renders its own real heading instead of a
 * second crumb (CAD-863 release correction).
 */
export default function CrmShell({
  scope,
  viewer,
  view,
  recordId,
  section,
  onView,
  onSelect,
  onRecordCreated,
}: {
  scope: HostScope;
  /** CAD-813: the operator's newest chat message daemon-stamped with
   *  this scope — passed through to Campaigns untouched. */
  viewer: Viewer;
  view: "list" | "new";
  recordId: string | null;
  section: CrmSection;
  onView: (view: "list" | "new") => void;
  onSelect: (recordId: string | null) => void;
  onRecordCreated?: (recordId: string) => void;
}) {
  const canWrite = viewer.operator && !viewer.readOnly;
  // The CSV import page is component-local page state under
  // Customers — it never enters the route URL (no customer content
  // or bulk bytes in history), so `record`/`view` still own the URL
  // grammar exactly as before.
  const [importing, setImporting] = useState(false);
  const [listRefresh, setListRefresh] = useState(0);
  // Section navigation lives in the host-owned outlet submenu
  // (CrmOutlet): this pane only renders the active section body.
  return (
    <div className="app-outlet crm" data-outlet="crm">

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

      {section === "customers" && view === "list" && (
        <CustomerList
          scope={scope}
          viewer={viewer}
          refresh={listRefresh}
          onSelect={onSelect}
          onNew={() => {
            setImporting(false);
            onView("new");
          }}
          onImport={() => {
            setImporting(true);
            onView("new");
          }}
        />
      )}
      {section === "customers" && view === "new" && importing && (
        <CustomerCsvImport
          scope={scope}
          viewer={viewer}
          onDone={() => {
            // A committed import re-reads the list from the server —
            // never trust local state over the rows the host stores.
            setListRefresh((count) => count + 1);
            setImporting(false);
            onView("list");
          }}
          onCancel={() => {
            setImporting(false);
            onView("list");
          }}
        />
      )}
      {section === "customers" && view === "new" && !importing && (
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
          onCancel={() => {
            setImporting(false);
            onView("list");
          }}
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
  refresh,
  onSelect,
  onNew,
  onImport,
}: {
  scope: HostScope;
  viewer: Viewer;
  /** Bumped after a committed CSV import — re-reads server rows. */
  refresh: number;
  onSelect: (recordId: string) => void;
  onNew: () => void;
  onImport: () => void;
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

  const reloadToken = `${scope.installId}:${scope.contextId}:${committed}:${cursor ?? ""}:${retry}:${refresh}`;
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
      <h3 className="text-cardtitle font-medium text-ink-100" data-outlet-heading>
        Customers
      </h3>
      <div className="crm-toolbar mb-1">
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
          <>
            <Button size="sm" onClick={onImport}>
              Import CSV
            </Button>
            <Button variant="primary" size="sm" onClick={onNew}>
              New customer
            </Button>
          </>
        )}
      </div>
      {!viewer.operator && (
        <Notice>Sign in as the operator to inspect customer records.</Notice>
      )}
      {viewer.operator && viewer.readOnly && (
        <Notice state="read-only">Read-only view. Record creation and edits are unavailable.</Notice>
      )}
      {scope.contextId === "" && viewer.operator && (
        <Notice>Administrator CRM setup is required before customers open.</Notice>
      )}
      {scope.contextId !== "" && viewer.operator && loading && (
        <Loading>Reading customers…</Loading>
      )}
      {scope.contextId !== "" && viewer.operator && error !== null && !loading && (
        <ErrorNotice onRetry={() => setRetry((count) => count + 1)}>{error}</ErrorNotice>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && !loading && records.length === 0 && (
        <EmptyState
          name="customers"
          title={committed === "" ? "No customers yet" : "No customers match this search"}
        >
          {committed === ""
            ? "Create the first record with New customer, or import a CSV. Only real server rows appear here."
            : "Clear the search to see every record."}
        </EmptyState>
      )}
      {scope.contextId !== "" && viewer.operator && error === null && records.length > 0 && (
        <>
          <DataTable<HostRecord>
            label="Customers table — scroll horizontally to reach every column"
            wrapClassName="crm-table-wrap"
            tableClassName="crm-table"
            rowKey={(record) => record.id}
            rowProps={(record) => ({ "data-record-id": record.id })}
            columns={[
              {
                key: "name",
                header: "Name",
                cellClassName: "text-ink-100",
                cell: (record) => viewProfile(record.profile).displayName,
              },
              {
                key: "email",
                header: "Email",
                cellClassName: "num text-ink-300",
                cell: (record) => viewProfile(record.profile).email ?? "—",
              },
              {
                key: "tags",
                header: "Tags",
                cellClassName: "text-ink-300",
                cell: (record) => {
                  const view = viewProfile(record.profile);
                  return (
                    <>
                      {view.tags.length > 0 ? view.tags.join(", ") : "—"}
                      {view.source ? (
                        <span className="num text-micro text-ink-500"> · {view.source}</span>
                      ) : null}
                    </>
                  );
                },
              },
              {
                key: "consent",
                header: "Consent",
                cell: (record) => (
                  <span className="chip" title="Email consent">
                    {viewProfile(record.profile).consentEmail}
                  </span>
                ),
              },
              {
                key: "open",
                header: <span className="sr-only">Open</span>,
                cell: (record) => (
                  <button type="button" className="lnk" onClick={() => onSelect(record.id)}>
                    Open
                  </button>
                ),
              },
            ]}
            rows={records}
          />
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
  { value: "unknown", label: "Unknown" },
  { value: "granted", label: "Granted" },
  { value: "denied", label: "Denied" },
];

function CustomerForm({
  initial,
  submitLabel,
  pending,
  formError,
  formId,
  onSubmit,
}: {
  initial: CustomerFormFields;
  submitLabel: string;
  /** Set when a drawer footer owns the Save button; the form then has none. */
  formId?: string;
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
      id={formId}
      className={formId ? "grid gap-3" : "card px-4 py-4 grid gap-3"}
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
      <Field label="Display name" id="crm-display-name" required className="crm-field">
        {(c) => (
          <input
            {...c}
            className="field"
            value={fields.displayName}
            onChange={(e) => set({ displayName: e.target.value })}
            maxLength={120}
            autoComplete="off"
          />
        )}
      </Field>
      <div className="crm-field-row">
        <Field label="Email (optional)" id="crm-email" className="crm-field">
          {(c) => (
            <input
              {...c}
              className="field"
              type="email"
              value={fields.email}
              onChange={(e) => set({ email: e.target.value })}
              maxLength={254}
              autoComplete="off"
              placeholder="name@example.com"
            />
          )}
        </Field>
        <Field label="Phone (optional)" id="crm-phone" className="crm-field">
          {(c) => (
            <input
              {...c}
              className="field"
              type="tel"
              value={fields.phone}
              onChange={(e) => set({ phone: e.target.value })}
              maxLength={24}
              autoComplete="off"
            />
          )}
        </Field>
      </div>
      <div className="crm-field-row">
        <Field label="Tags (optional)" id="crm-tags" hint="Comma separated" className="crm-field">
          {(c) => (
            <input
              {...c}
              className="field"
              value={fields.tags}
              onChange={(e) => set({ tags: e.target.value })}
              maxLength={400}
              autoComplete="off"
              placeholder="vip, newsletter"
            />
          )}
        </Field>
        <Field label="Source (optional)" id="crm-source" className="crm-field">
          {(c) => (
            <input
              {...c}
              className="field"
              value={fields.source}
              onChange={(e) => set({ source: e.target.value })}
              maxLength={40}
              autoComplete="off"
              placeholder="import"
            />
          )}
        </Field>
      </div>
      <div className="crm-field-row">
        <Field label="Email consent (explicit)" id="crm-consent-email" hint="No marketing until granted" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={fields.consentEmail}
              onChange={(value) => isConsentChoice(value) && set({ consentEmail: value })}
              options={CONSENT_OPTIONS}
              aria-label="Email consent"
              full
            />
          )}
        </Field>
        <Field label="SMS consent (explicit)" id="crm-consent-sms" hint="No marketing until granted" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={fields.consentSms}
              onChange={(value) => isConsentChoice(value) && set({ consentSms: value })}
              options={CONSENT_OPTIONS}
              aria-label="SMS consent"
              full
            />
          )}
        </Field>
      </div>
      {(fieldError ?? formError) && (
        <ErrorNotice bare>{fieldError ?? formError}</ErrorNotice>
      )}
      {formId === undefined && (
        <div>
          <Button type="submit" variant="primary" loading={pending} disabled={pending}>
            {submitLabel}
          </Button>
        </div>
      )}
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
        — duplicates and stale writes are refused by the server.
      </p>
      {!canWrite ? (
        <Notice className="mt-2" state="read-only">
          Read-only view. A verified operator creates customer records.
        </Notice>
      ) : scope.contextId === "" ? (
        <Notice className="mt-2">Administrator CRM setup is required before creating a customer.</Notice>
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

  const initials = view
    ? view.displayName.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("")
    : "";
  const reload = () => {
    setError(null);
    setLoading(true);
    hostActions
      .show(scope, recordId)
      .then(setRecord)
      .catch((e: unknown) => setError(friendlyError(e)))
      .finally(() => setLoading(false));
  };
  const ready = !loading && error === null && record !== null && view !== null;

  const tabs: DrawerTab[] =
    ready && record !== null && view !== null
      ? [
          {
            id: "overview",
            label: "Overview",
            panel: (
              <>
                <p className="text-micro text-ink-500 mb-2">
                  Selection is chat context only — the server re-proves scope on every send.
                </p>
                <Detail
                  className="crm-detail"
                  label="Customer fields"
                  items={[
                    { key: "email", term: "Email", value: <span className="num">{view.email ?? "—"}</span> },
                    { key: "phone", term: "Phone", value: <span className="num">{view.phone ?? "—"}</span> },
                    { key: "tags", term: "Tags", value: view.tags.length > 0 ? view.tags.join(", ") : "—" },
                    { key: "source", term: "Source", value: view.source ?? "—" },
                    {
                      key: "consent-email",
                      term: "Email consent",
                      value: <span className="chip">{view.consentEmail}</span>,
                    },
                    {
                      key: "consent-sms",
                      term: "SMS consent",
                      value: <span className="chip">{view.consentSms ?? "unknown"}</span>,
                    },
                  ]}
                />
              </>
            ),
          },
          {
            id: "activity",
            label: "Activity",
            panel: (
              <>
                {consentTrail.length > 0 && (
                  <section aria-label="Consent history">
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
                {consentTrail.length === 0 && revisions.length === 0 && (
                  <p className="text-secondary text-ink-400">No activity recorded yet.</p>
                )}
              </>
            ),
          },
          {
            id: "details",
            label: "Details",
            panel: (
              <section aria-label="Record diagnostics">
                <p className="num text-micro text-ink-500">
                  Record <span className="num">{recordId}</span> · revision r{record.revision} · digest{" "}
                  {record.digest.slice(0, 18)}… · scope <span className="num">{scope.contextId || "none"}</span>
                </p>
              </section>
            ),
          },
        ]
      : [];

  return (
    <DrawerShell
      kind="customer"
      label="Customer details"
      title={loading ? "Customer details" : (view?.displayName ?? "Customer details")}
      avatar={initials || undefined}
      subtitle={view?.email ?? undefined}
      pills={
        view && ready ? (
          <>
            <span className="chip" title="Email consent">Email: {view.consentEmail}</span>
            <span className="chip" title="SMS consent">SMS: {view.consentSms ?? "unknown"}</span>
            {view.tags.map((tag) => (
              <span key={tag} className="chip">{tag}</span>
            ))}
          </>
        ) : undefined
      }
      tabs={tabs}
      state={
        loading ? (
          <Loading>Reading the record…</Loading>
        ) : error !== null ? (
          <ErrorNotice onRetry={reload}>{error}</ErrorNotice>
        ) : undefined
      }
      edit={
        editing && canWrite && ready && record !== null
          ? {
              formId: "crm-customer-edit",
              title: "Edit profile — saving is refused if the record changed since you opened it",
              pending,
              onCancel: () => setEditing(false),
              body: (
                <CustomerForm
                  key={record.revision}
                  formId="crm-customer-edit"
                  initial={formFromProfile(record.profile)}
                  submitLabel="Save changes"
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
              ),
            }
          : null
      }
      menu={
        ready && view?.email
          ? [{ key: "copy-email", label: "Copy email", onSelect: () => void navigator.clipboard?.writeText(view.email ?? "") }]
          : []
      }
      primary={
        ready && canWrite ? (
          <Button size="sm" variant="primary" onClick={() => { setEditing(true); setFormError(null); }}>
            Edit profile
          </Button>
        ) : undefined
      }
      note={
        ready && !canWrite
          ? `Read-only view. ${viewer.operator ? "Edits are disabled on this board." : "Sign in as the operator to edit."}`
          : undefined
      }
      onClose={onClose}
    />
  );
}
