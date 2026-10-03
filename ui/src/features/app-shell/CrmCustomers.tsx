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
  activityItems,
  buildConsentChange,
  buildCustomerProfile,
  CONSENT_METHODS,
  CONSENT_NOTE_MAX,
  consentLabel,
  type ConsentSetting,
  EMPTY_CUSTOMER_FORM,
  formFromProfile,
  friendlyError,
  isConsentChoice,
  newCustomerId,
  viewProfile,
  type CustomerFormFields,
} from "./customerProfile";
import { hostActions, type ConsentMethod, type HostRecord, type HostScope } from "./hostActions";

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
  // Filters and selection act on the rows already read (this page);
  // the server search stays the only whole-list narrowing.
  const [consentFilter, setConsentFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState("");
  const [tagFilter, setTagFilter] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const sources = Array.from(
    new Set(records.map((r) => viewProfile(r.profile).source).filter((v): v is string => v !== null)),
  ).sort();
  const shown = records.filter((r) => {
    const v = viewProfile(r.profile);
    return (
      (consentFilter === "" || v.consentEmail === consentFilter) &&
      (sourceFilter === "" || v.source === sourceFilter) &&
      (tagFilter === "" || v.tags.includes(tagFilter))
    );
  });
  // Selection never outlives the rows it named.
  useEffect(() => {
    setSelected(new Set());
  }, [records]);

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
          <div className="crm-filters" role="group" aria-label="Filter customers on this page">
            <Select
              value={consentFilter}
              onChange={setConsentFilter}
              options={[
                { value: "", label: "Email consent: any" },
                { value: "granted", label: "Granted" },
                { value: "unknown", label: "Unknown" },
                { value: "denied", label: "Withdrawn" },
              ]}
              aria-label="Filter by email consent"
            />
            <Select
              value={sourceFilter}
              onChange={setSourceFilter}
              options={[{ value: "", label: "Source: any" }, ...sources.map((v) => ({ value: v, label: v }))]}
              aria-label="Filter by source"
            />
            {tagFilter !== "" && (
              <button type="button" className="chip crm-chip-on" onClick={() => setTagFilter("")} aria-label={`Clear tag filter ${tagFilter}`}>
                tag: {tagFilter} ✕
              </button>
            )}
          </div>
          {selected.size > 0 && (
            <p className="crm-selection text-label text-ink-300" role="status">
              {selected.size} selected{" "}
              <button type="button" className="lnk" onClick={() => setSelected(new Set())}>
                Clear
              </button>
            </p>
          )}
          {shown.length === 0 && (
            <EmptyState name="customers" title="No customers on this page match the filters">
              Clear a filter to see this page's rows again.
            </EmptyState>
          )}
          {shown.length > 0 && (
          <DataTable<HostRecord>
            label="Customers table — scroll horizontally to reach every column"
            wrapClassName="crm-table-wrap"
            tableClassName="crm-table"
            rowKey={(record) => record.id}
            rowProps={(record) => ({
              "data-record-id": record.id,
              className: "crm-row-open",
              onClick: (e: { target: EventTarget }) => {
                // A click on a control inside the row keeps its own meaning.
                if ((e.target as HTMLElement).closest("button, input, a")) return;
                onSelect(record.id);
              },
            })}
            columns={[
              {
                key: "select",
                header: (
                  <input
                    type="checkbox"
                    aria-label="Select all customers on this page"
                    checked={shown.length > 0 && shown.every((r) => selected.has(r.id))}
                    onChange={(e) =>
                      setSelected(e.target.checked ? new Set(shown.map((r) => r.id)) : new Set())
                    }
                  />
                ),
                cell: (record) => (
                  <input
                    type="checkbox"
                    aria-label={`Select ${viewProfile(record.profile).displayName}`}
                    checked={selected.has(record.id)}
                    onChange={(e) =>
                      setSelected((prev) => {
                        const next = new Set(prev);
                        if (e.target.checked) next.add(record.id);
                        else next.delete(record.id);
                        return next;
                      })
                    }
                  />
                ),
              },
              {
                key: "name",
                header: "Name",
                cellClassName: "text-ink-100",
                cell: (record) => (
                  <button type="button" className="lnk" onClick={() => onSelect(record.id)}>
                    {viewProfile(record.profile).displayName}
                  </button>
                ),
              },
              {
                key: "email",
                header: "Email",
                cellClassName: "num text-ink-300 crm-nowrap",
                cell: (record) => viewProfile(record.profile).email ?? "—",
              },
              {
                key: "tags",
                header: "Tags",
                cell: (record) => {
                  const tags = viewProfile(record.profile).tags;
                  return tags.length > 0 ? (
                    <span className="crm-chips">
                      {tags.map((tag) => (
                        <button key={tag} type="button" className="chip" title={`Filter by tag ${tag}`} onClick={() => setTagFilter(tag)}>
                          {tag}
                        </button>
                      ))}
                    </span>
                  ) : (
                    "—"
                  );
                },
              },
              {
                key: "source",
                header: "Source",
                cellClassName: "text-ink-300",
                cell: (record) => viewProfile(record.profile).source ?? "—",
              },
              {
                key: "consent",
                header: "Email consent",
                cell: (record) => {
                  const state = viewProfile(record.profile).consentEmail;
                  return (
                    <span className={`chip ${state === "granted" ? "crm-consent-ok" : ""}`} title="Email consent">
                      {consentLabel(state)}
                    </span>
                  );
                },
              },
            ]}
            rows={shown}
          />
          )}
          <div className="crm-pager">
            <Button
              size="sm"
              disabled={cursors.length === 0 || loading}
              onClick={() => setCursors((prev) => prev.slice(0, -1))}
            >
              ← Previous
            </Button>
            <span className="num text-micro text-ink-500" aria-live="polite">
              {shown.length === records.length ? "" : `${shown.length} of `}
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
  hideConsent,
  onSubmit,
}: {
  initial: CustomerFormFields;
  submitLabel: string;
  /** Set when a drawer footer owns the Save button; the form then has none. */
  formId?: string;
  /** Edit hides consent: it changes only through Record consent, with provenance. */
  hideConsent?: boolean;
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
      {!hideConsent && (
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
      )}
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

const CONSENT_SETTINGS: { value: string; label: string }[] = [
  { value: "granted", label: "Granted" },
  { value: "unknown", label: "Unknown" },
  { value: "denied", label: "Withdrawn" },
];

function asSetting(value: string | null): ConsentSetting {
  return value === "granted" || value === "denied" ? value : "unknown";
}

/** Record consent: Email/SMS status, how it was given, optional note. */
function ConsentForm({
  profile,
  formId,
  formError,
  onSubmit,
}: {
  profile: unknown;
  formId: string;
  formError: string | null;
  onSubmit: (change: ReturnType<typeof buildConsentChange>) => void;
}) {
  const current = viewProfile(profile);
  const [email, setEmail] = useState<ConsentSetting>(asSetting(current.consentEmail));
  const [sms, setSms] = useState<ConsentSetting>(asSetting(current.consentSms));
  const [method, setMethod] = useState<ConsentMethod | "">("");
  const [note, setNote] = useState("");
  const [fieldError, setFieldError] = useState<string | null>(null);
  const granting =
    (email === "granted" && current.consentEmail !== "granted") ||
    (sms === "granted" && current.consentSms !== "granted");
  return (
    <form
      id={formId}
      className="grid gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        try {
          onSubmit(buildConsentChange(profile, { email, sms, method: granting ? method : "", note }));
        } catch (error: unknown) {
          setFieldError(error instanceof ApiError ? error.message : String(error));
        }
      }}
    >
      <div className="crm-field-row">
        <Field label="Email marketing" id="crm-rc-email" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={email}
              onChange={(v) => { setEmail(asSetting(v)); setFieldError(null); }}
              options={CONSENT_SETTINGS}
              aria-label="Email marketing consent"
              full
            />
          )}
        </Field>
        <Field label="SMS marketing" id="crm-rc-sms" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={sms}
              onChange={(v) => { setSms(asSetting(v)); setFieldError(null); }}
              options={CONSENT_SETTINGS}
              aria-label="SMS marketing consent"
              full
            />
          )}
        </Field>
      </div>
      {granting && (
        <Field label="How was it given?" id="crm-rc-method" hint="Kept in the customer's activity for audit" className="crm-field">
          {(c) => (
            <Select
              id={c.id}
              value={method}
              onChange={(v) => { setMethod(v as ConsentMethod | ""); setFieldError(null); }}
              options={[{ value: "", label: "Choose…" }, ...CONSENT_METHODS]}
              aria-label="How consent was given"
              full
            />
          )}
        </Field>
      )}
      <Field label="Note (optional)" id="crm-rc-note" className="crm-field">
        {(c) => (
          <input
            {...c}
            className="field"
            value={note}
            onChange={(e) => { setNote(e.target.value); setFieldError(null); }}
            maxLength={CONSENT_NOTE_MAX}
            autoComplete="off"
            placeholder="e.g. Signed at the counter, 2 Oct"
          />
        )}
      </Field>
      {(fieldError ?? formError) && <ErrorNotice bare>{fieldError ?? formError}</ErrorNotice>}
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
  const [mode, setMode] = useState<"view" | "edit" | "consent">("view");
  const [pending, setPending] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const reloadToken = `${scope.installId}:${scope.contextId}:${recordId}`;
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setMode("view");
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
  const timeline = record ? activityItems(record) : [];

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

  const when = (at: number) => new Date(at * 1000).toISOString().slice(0, 10);
  const tabs: DrawerTab[] =
    ready && record !== null && view !== null
      ? [
          {
            id: "overview",
            label: "Overview",
            panel: (
              <>
                <section className="crm-sect" aria-label="Consent">
                  <h4>Consent</h4>
                  <div className="crm-consent-cards">
                    <div className="card px-3 py-2" data-consent="email">
                      <div className="text-label text-ink-300">Email marketing</div>
                      <span className={`chip ${view.consentEmail === "granted" ? "crm-consent-ok" : ""}`}>{consentLabel(view.consentEmail)}</span>
                      <div className="text-micro text-ink-500">
                        {view.consentEmail === "granted" ? "Can receive campaigns" : "No marketing until granted"}
                      </div>
                    </div>
                    <div className="card px-3 py-2" data-consent="sms">
                      <div className="text-label text-ink-300">SMS marketing</div>
                      <span className={`chip ${view.consentSms === "granted" ? "crm-consent-ok" : ""}`}>{consentLabel(view.consentSms)}</span>
                      <div className="text-micro text-ink-500">{view.consentSms === null ? "Not recorded" : "Per customer choice"}</div>
                    </div>
                  </div>
                </section>
                <section className="crm-sect" aria-label="Profile">
                  <h4>Profile</h4>
                  <Detail
                    className="crm-detail"
                    label="Customer fields"
                    items={[
                      { key: "email", term: "Email", value: <span className="num">{view.email ?? "—"}</span> },
                      { key: "phone", term: "Phone", value: <span className="num">{view.phone ?? "—"}</span> },
                      { key: "tags", term: "Tags", value: view.tags.length > 0 ? view.tags.join(", ") : "—" },
                      { key: "source", term: "Source", value: view.source ?? "—" },
                    ]}
                  />
                </section>
                <section className="crm-sect" aria-label="Segments">
                  <h4>Segments</h4>
                  <p className="text-secondary text-ink-400">Membership is shown on each segment.</p>
                </section>
                <section className="crm-sect" aria-label="Campaigns">
                  <h4>Campaigns</h4>
                  <p className="text-secondary text-ink-400">Send history is shown on each campaign.</p>
                </section>
              </>
            ),
          },
          {
            id: "activity",
            label: "Activity",
            panel:
              timeline.length > 0 ? (
                <ol className="crm-timeline" aria-label="Activity">
                  {timeline.map((item) => (
                    <li key={item.key}>
                      <span>{item.text}</span>
                      {item.note && <span className="text-micro text-ink-400"> — “{item.note}”</span>}
                      <div className="num text-micro text-ink-500">{when(item.at)}</div>
                    </li>
                  ))}
                </ol>
              ) : (
                <p className="text-secondary text-ink-400">No activity recorded yet.</p>
              ),
          },
          {
            id: "details",
            label: "Details",
            panel: (
              <section aria-label="Record diagnostics">
                <p className="text-micro text-ink-500 mb-2">
                  Selection is chat context only — the server re-proves scope on every send.
                </p>
                <p className="num text-micro text-ink-500">
                  Record <span className="num">{recordId}</span> · revision r{record.revision} · digest{" "}
                  {record.digest.slice(0, 18)}… · scope <span className="num">{scope.contextId || "none"}</span>
                </p>
              </section>
            ),
          },
        ]
      : [];

  const save = (profile: Record<string, unknown>, provenance?: { method: ConsentMethod; note?: string }) => {
    if (record === null) return;
    setPending(true);
    setFormError(null);
    void hostActions
      .update(scope, recordId, record.revision, profile, provenance)
      .then((next) => {
        setRecord(next);
        setMode("view");
      })
      .catch((e: unknown) => setFormError(friendlyError(e)))
      .finally(() => setPending(false));
  };

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
      warning={
        ready && view !== null && view.consentEmail !== "granted" ? (
          <>
            <b>{view.displayName} can't receive campaigns.</b>{" "}
            {view.consentEmail === "denied" ? "Email consent was withdrawn." : "No email consent recorded."}
          </>
        ) : undefined
      }
      edit={
        mode !== "view" && canWrite && ready && record !== null
          ? mode === "edit"
            ? {
                formId: "crm-customer-edit",
                title: "Edit profile — saving is refused if the record changed since you opened it",
                pending,
                onCancel: () => setMode("view"),
                body: (
                  <CustomerForm
                    key={record.revision}
                    formId="crm-customer-edit"
                    hideConsent
                    initial={formFromProfile(record.profile)}
                    submitLabel="Save changes"
                    pending={pending}
                    formError={formError}
                    onSubmit={(fields) => {
                      let profile: Record<string, unknown>;
                      try {
                        profile = buildCustomerProfile(fields);
                      } catch (e: unknown) {
                        setFormError(friendlyError(e));
                        return;
                      }
                      save(profile);
                    }}
                  />
                ),
              }
            : {
                formId: "crm-customer-consent",
                title: "Record consent",
                saveLabel: "Save consent",
                pending,
                onCancel: () => setMode("view"),
                body: (
                  <ConsentForm
                    key={record.revision}
                    formId="crm-customer-consent"
                    profile={record.profile}
                    formError={formError}
                    onSubmit={(change) => save(change.profile, change.provenance ?? undefined)}
                  />
                ),
              }
          : null
      }
      menu={
        ready
          ? [
              ...(view?.email
                ? [{ key: "copy-email", label: "Copy email", onSelect: () => void navigator.clipboard?.writeText(view.email ?? "") }]
                : []),
              { key: "outbox", label: "View in Outbox", onSelect: () => {}, disabled: true, title: "Per-customer Outbox view is not available yet" },
              ...(canWrite
                ? [{ key: "archive", label: "Archive customer", onSelect: () => {}, destructive: true, disabled: true, title: "Archiving customers is not available yet" }]
                : []),
            ]
          : []
      }
      secondary={
        ready && canWrite ? (
          <>
            <Button size="sm" disabled title="Adding to a segment from here is not available yet">
              Add to segment
            </Button>
            <Button size="sm" onClick={() => { setMode("edit"); setFormError(null); }}>
              Edit
            </Button>
          </>
        ) : undefined
      }
      primary={
        ready && canWrite ? (
          <Button size="sm" variant="primary" onClick={() => { setMode("consent"); setFormError(null); }}>
            Record consent
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
