import { useEffect, useId, useMemo, useState, type FormEvent } from "react";
import type { AppViewCell, AppViewRow, AppViewView } from "./contract";
import { liveRows } from "./contract";
import { AppViewCellRenderer } from "./AppView";
import type { AppViewReceipt, ResolvedLiveView } from "./viewReceipt";
import { isSafeRecordId } from "./appBinding";
import { workspaceApps } from "../../workspace-apps/workspaceApps";
import DataTable from "../shared/DataTable";
import Detail from "../shared/Detail";
import { EmptyState, ErrorNotice, Loading, Notice } from "../shared/States";
import Link from "../../../ui/Link";

const PAGE_LIMIT = 50;
const MAX_LIVE_ROWS = 1000;

interface ReadState {
  key: string;
  status: "loading" | "success" | "error";
  rows: AppViewRow[];
  truncated: boolean;
  nextCursor: string | null;
  error: string | null;
}

interface ReadPage {
  rows: AppViewRow[];
  truncated: boolean;
  nextCursor: string | null;
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null)
    ? value as Record<string, unknown> : null;
}

function parseReadPage(
  raw: unknown,
  receipt: AppViewReceipt,
  route: ResolvedLiveView,
  cursor: string | null,
): ReadPage {
  const body = record(raw);
  if (!body) throw new Error("The live read returned an invalid receipt.");
  const allowed = ["rows", "view_id", "op", "digest", "view_descriptor_digest", "view_binding_digest"];
  if (route.op === "list" && route.binding.source === "customers") allowed.push("truncated", "next_cursor");
  if (Object.keys(body).some((key) => !allowed.includes(key))) {
    throw new Error("The live read receipt contains an unsupported field.");
  }
  if (body.view_id !== route.view.id || body.op !== route.op
      || body.digest !== receipt.bundleDigest
      || body.view_descriptor_digest !== receipt.descriptorDigest
      || body.view_binding_digest !== receipt.bindingDigest) {
    throw new Error("The live read receipt does not match the installed view pins.");
  }
  const rows = liveRows(route.view, body.rows);
  const boundFields = new Set(route.binding.fields.map((field) => field.field));
  if (rows.some((row) => Object.keys(row).some((key) => !boundFields.has(key)))) {
    throw new Error("The live read returned a field outside this installed binding.");
  }
  if (route.op === "show" && rows.length !== 1) throw new Error("The detail read did not return exactly one record.");
  if (route.op === "list" && route.binding.source === "customers") {
    if (rows.length > PAGE_LIMIT || typeof body.truncated !== "boolean") {
      throw new Error("The customer page exceeded its declared bound.");
    }
    const nextCursor = body.next_cursor === null ? null : body.next_cursor;
    if (nextCursor !== null && !isSafeRecordId(nextCursor)) throw new Error("The customer page cursor is invalid.");
    if (body.truncated && (nextCursor === null || nextCursor === cursor)) {
      throw new Error("The customer page did not provide a fresh next cursor.");
    }
    if (!body.truncated && nextCursor !== null) throw new Error("The customer page returned an unexpected cursor.");
    return { rows, truncated: body.truncated, nextCursor };
  }
  return { rows, truncated: false, nextCursor: null };
}

function validateRowIdentity(rows: AppViewRow[], field: string | null): void {
  if (field === null) return;
  const seen = new Set<string>();
  for (const row of rows) {
    const id = row[field];
    if (!isSafeRecordId(id)) throw new Error("A live row is missing its bound source identity.");
    if (seen.has(id)) throw new Error("The live read returned a duplicate source identity.");
    seen.add(id);
  }
}

function fields(view: AppViewView) {
  return new Map((view.fields ?? []).map((field) => [field.id, field]));
}

function renderCell(field: NonNullable<AppViewView["fields"]>[number], value: AppViewCell | undefined) {
  return <AppViewCellRenderer field={field} value={value} />;
}

export default function LiveAppView({
  installId,
  contextId,
  receipt,
  route,
  enabled,
  blockedReason,
  returnHref,
  backHref,
  detailHref,
}: {
  installId: string;
  contextId: string;
  receipt: AppViewReceipt;
  route: ResolvedLiveView;
  enabled: boolean;
  blockedReason: string | null;
  returnHref: string;
  backHref: string | null;
  detailHref: (viewId: string, recordId: string) => string;
}) {
  const searchId = useId();
  const [searchDraft, setSearchDraft] = useState("");
  const [appliedQuery, setAppliedQuery] = useState<string | null>(null);
  const [queryError, setQueryError] = useState<string | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [readState, setReadState] = useState<ReadState | null>(null);
  const canSearch = route.op === "list" && route.binding.source === "customers";
  const requestKey = useMemo(() => JSON.stringify([
    installId,
    contextId,
    route.view.id,
    route.op,
    route.recordId,
    receipt.bundleDigest,
    receipt.descriptorDigest,
    receipt.bindingDigest,
    appliedQuery,
  ]), [
    installId,
    contextId,
    route.view.id,
    route.op,
    route.recordId,
    receipt.bundleDigest,
    receipt.descriptorDigest,
    receipt.bindingDigest,
    appliedQuery,
  ]);

  useEffect(() => {
    if (!enabled || !contextId) return;
    const controller = new AbortController();
    setReadState((previous) => {
      const keepPageRows = cursor !== null && previous?.key === requestKey && previous.rows.length > 0;
      return {
        key: requestKey,
        status: "loading",
        rows: keepPageRows ? previous.rows : [],
        truncated: keepPageRows ? previous.truncated : false,
        nextCursor: keepPageRows ? previous.nextCursor : null,
        error: null,
      };
    });
    workspaceApps.viewRead({
      installId,
      viewId: route.view.id,
      ...(route.recordId === null ? {} : { recordId: route.recordId }),
      bundleDigest: receipt.bundleDigest,
      descriptorDigest: receipt.descriptorDigest!,
      bindingDigest: receipt.bindingDigest!,
      contextId,
      source: route.binding.source,
      ...(canSearch ? { limit: PAGE_LIMIT, query: appliedQuery ?? undefined, cursor: cursor ?? undefined } : {}),
    }, controller.signal)
      .then((raw) => {
        if (controller.signal.aborted) return;
        const page = parseReadPage(raw, receipt, route, cursor);
        validateRowIdentity(page.rows, route.op === "list" ? route.identityField : null);
        setReadState((previous) => {
          const rows = cursor !== null && previous?.key === requestKey
            ? [...previous.rows, ...page.rows]
            : page.rows;
          try {
            if (rows.length > MAX_LIVE_ROWS) throw new Error("The live view exceeds the client row bound.");
            validateRowIdentity(rows, route.op === "list" ? route.identityField : null);
            return {
              key: requestKey,
              status: "success",
              rows,
              truncated: page.truncated,
              nextCursor: page.nextCursor,
              error: null,
            };
          } catch (error) {
            return {
              key: requestKey,
              status: "error",
              rows: cursor !== null && previous?.key === requestKey ? previous.rows : [],
              truncated: false,
              nextCursor: null,
              error: error instanceof Error ? error.message : "The live view exceeds its bound.",
            };
          }
        });
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setReadState((previous) => ({
          key: requestKey,
          status: "error",
          rows: cursor !== null && previous?.key === requestKey ? previous.rows : [],
          truncated: false,
          nextCursor: null,
          error: error instanceof Error ? error.message : "Could not read this live view.",
        }));
      });
    return () => controller.abort();
  }, [
    enabled,
    contextId,
    installId,
    route,
    receipt,
    canSearch,
    appliedQuery,
    cursor,
    requestKey,
    retry,
  ]);

  const current = readState?.key === requestKey ? readState : null;
  const onSearch = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const value = searchDraft.trim();
    if (value && (new TextEncoder().encode(value).byteLength > 120 || /[\u0000-\u001f\u007f]/.test(value))) {
      setQueryError("Search is limited to 120 UTF-8 bytes and cannot contain control characters.");
      return;
    }
    setQueryError(null);
    setAppliedQuery(value || null);
    setCursor(null);
    setRetry((count) => count + 1);
  };
  const retryRead = () => setRetry((count) => count + 1);

  if (!enabled) {
    return (
      <div className="av" data-live-view={route.view.id}>
        <Notice state="live-read-disabled">
          {blockedReason ?? "An active app context is required before live records can be read."}{" "}
          <Link href={returnHref} className="lnk">Return to the installed app</Link>
        </Notice>
      </div>
    );
  }

  const fieldMap = fields(route.view);
  const identityField = route.identityField;
  const openDetail = route.op === "list" && route.detailViewId !== null && route.rowNavigationError === null;
  const boundFields = new Set(route.binding.fields.map((field) => field.field));
  const columns = (route.view.columns ?? []).filter((column) => boundFields.has(column.field)).map((column) => ({
    key: column.field,
    header: column.label ?? fieldMap.get(column.field)!.label,
    cell: (row: AppViewRow) => renderCell(fieldMap.get(column.field)!, row[column.field]),
  }));
  if (openDetail) {
    columns.push({
      key: "open-detail",
      header: "Record",
      cell: (row: AppViewRow) => {
        const id = identityField ? row[identityField] : undefined;
        return isSafeRecordId(id)
          ? <Link href={detailHref(route.detailViewId!, id)} className="lnk">Open</Link>
          : <span className="text-ink-500">Unavailable</span>;
      },
    });
  }

  return (
    <section className="av" aria-labelledby={`${searchId}-title`} data-live-view={route.view.id}>
      <header className="av-head">
        <h3 id={`${searchId}-title`} className="text-cardtitle font-medium text-ink-100" tabIndex={-1}>
          {route.view.title}
        </h3>
        <p className="num text-micro text-ink-500">
          {receipt.descriptor?.app} · live {route.view.kind} · read-only
        </p>
      </header>
      <Notice state="live-read-only">
        Live records are read through this installation's verified view and binding receipt. Forms and actions remain disabled.
      </Notice>

      {route.op === "show" && backHref && route.tableView && (
        <p><Link href={backHref} className="lnk">← Back to {route.tableView.title}</Link></p>
      )}

      {canSearch && (
        <form className="live-view-search" onSubmit={onSearch} role="search" aria-label={`Search ${route.view.title}`}>
          <label htmlFor={searchId} className="text-label text-ink-300">Search records</label>
          <div className="live-view-search-row">
            <input
              id={searchId}
              type="search"
              className="av-input"
              value={searchDraft}
              maxLength={120}
              autoComplete="off"
              onChange={(event) => setSearchDraft(event.target.value)}
              aria-describedby={queryError ? `${searchId}-error` : undefined}
            />
            <button type="submit" className="btn btn-secondary btn-sm">Search</button>
            {appliedQuery !== null && (
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                onClick={() => {
                  setSearchDraft("");
                  setAppliedQuery(null);
                  setCursor(null);
                  setQueryError(null);
                  setRetry((count) => count + 1);
                }}
              >
                Clear
              </button>
            )}
          </div>
          {queryError && <p id={`${searchId}-error`} className="text-label text-fail" role="alert">{queryError}</p>}
        </form>
      )}

      {current?.status === "loading" && <Loading>{cursor ? `Loading more ${route.view.title}…` : `Reading ${route.view.title}…`}</Loading>}
      {current?.status === "error" && (
        <ErrorNotice onRetry={retryRead}>Could not read this installed view: {current.error}</ErrorNotice>
      )}
      {current?.status === "success" && current.rows.length === 0 && (
        <EmptyState title="No matching records" name={route.view.id}>
          The installed source returned no rows for this view and active context.
        </EmptyState>
      )}
      {current && current.rows.length > 0 && route.op === "list" && (
        <>
          <DataTable<AppViewRow>
            label={`${route.view.title} live records`}
            wrapClassName="av-table-wrap"
            tableClassName="av-table"
            rowKey={(row, index) => identityField && isSafeRecordId(row[identityField]) ? row[identityField] : index}
            columns={columns}
            rows={current.rows}
          />
          {route.rowNavigationError && <Notice state="detail-navigation-disabled">{route.rowNavigationError}</Notice>}
        </>
      )}
      {current?.status === "success" && current.rows.length > 0 && route.op === "show" && (
        <Detail
          className="av-detail"
          label={`${route.view.title} fields`}
          items={(route.view.fields ?? []).filter((field) => boundFields.has(field.id)).map((field) => ({
            key: field.id,
            term: field.label,
            value: <AppViewCellRenderer field={field} value={current.rows[0][field.id]} />,
          }))}
        />
      )}
      {current && current.rows.length > 0 && current.truncated && current.nextCursor && (
        <button
          type="button"
          className="btn btn-secondary btn-sm live-view-more"
          disabled={current.status === "loading"}
          onClick={() => setCursor(current.nextCursor)}
        >
          Load more
        </button>
      )}
    </section>
  );
}
