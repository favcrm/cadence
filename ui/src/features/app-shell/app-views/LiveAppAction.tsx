import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from "react";
import { liveRows, type AppViewCell } from "./contract";
import type { AppActionFieldV2 } from "./appAction";
import type { AppViewReceipt, ResolvedLiveAction } from "./viewReceipt";
import { isSafeRecordId } from "./appBinding";
import { workspaceApps } from "../../workspace-apps/workspaceApps";
import { navigate } from "../../../lib/useLocation";
import Field from "../shared/Field";
import { ErrorNotice, Loading, Notice } from "../shared/States";
import Link from "../../../ui/Link";

interface Draft {
  display_name: string;
  email: string;
  phone: string;
  source: string;
  tags: string;
}

interface ReadState {
  key: string;
  status: "loading" | "success" | "error";
  revision: number | null;
  error: string | null;
}

const EMPTY_DRAFT: Draft = { display_name: "", email: "", phone: "", source: "", tags: "" };
const RECORD_KEYS = ["id", "install_id", "context_id", "kind", "revision", "digest", "profile", "history", "consent_history"];
const PROFILE_KEYS = ["schema", "display_name", "email", "phone", "source", "tags", "consent"];
const SHA256 = /^sha256:[0-9a-f]{64}$/;

type Obj = Record<string, unknown>;

function object(value: unknown): Obj | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null)
    ? value as Obj : null;
}

function typedCell(value: AppViewCell | undefined, type: AppActionFieldV2["type"]): string {
  if (type === "tags") {
    if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
      throw new Error("The detail receipt is missing its bound tags list.");
    }
    return value.join(", ");
  }
  if (value === undefined) return "";
  if (typeof value !== "string") throw new Error("The detail receipt has an invalid editable text field.");
  return value;
}

function detailDraft(raw: unknown, receipt: AppViewReceipt, route: ResolvedLiveAction): { draft: Draft; revision: number } {
  const detail = route.detailRoute;
  if (!detail || !receipt.descriptorDigest || !receipt.bindingDigest) throw new Error("The update route has no verified customer detail read.");
  const body = object(raw);
  if (!body) throw new Error("The live detail returned an invalid receipt.");
  const allowed = ["rows", "view_id", "op", "digest", "view_descriptor_digest", "view_binding_digest", "record_revision"];
  if (Object.keys(body).some((key) => !allowed.includes(key))) throw new Error("The live detail receipt contains an unsupported field.");
  if (body.view_id !== detail.view.id || body.op !== "show"
      || body.digest !== receipt.bundleDigest
      || body.view_descriptor_digest !== receipt.descriptorDigest
      || body.view_binding_digest !== receipt.bindingDigest) {
    throw new Error("The live detail receipt does not match the installed view pins.");
  }
  const revision = body.record_revision;
  if (!Number.isSafeInteger(revision) || (revision as number) < 1 || (revision as number) >= Number.MAX_SAFE_INTEGER) {
    throw new Error("The live detail did not return a valid host record revision.");
  }
  const rows = liveRows(detail.view, body.rows);
  if (rows.length !== 1) throw new Error("The update route did not return exactly one customer.");
  const row = rows[0];
  const boundFields = new Set(detail.binding.fields.map((field) => field.field));
  if (Object.keys(row).some((key) => !boundFields.has(key))) throw new Error("The detail read returned a field outside its installed binding.");
  const byInput = (field: AppActionFieldV2): AppViewCell | undefined => {
    const bindings = detail.binding.fields.filter((binding) => binding.key === field.id);
    if (bindings.length !== 1) throw new Error(`The detail binding does not uniquely project ${field.id}.`);
    return row[bindings[0].field];
  };
  const values = new Map(route.action.input.fields.map((field) => [field.id, typedCell(byInput(field), field.type)]));
  const displayName = values.get("display_name");
  if (!displayName) throw new Error("The detail receipt is missing its bound display name.");
  return {
    draft: {
      display_name: displayName,
      email: values.get("email") ?? "",
      phone: values.get("phone") ?? "",
      source: values.get("source") ?? "",
      tags: values.get("tags") ?? "",
    },
    revision: revision as number,
  };
}

function effectiveTextLimit(field: AppActionFieldV2): number {
  const host = field.id === "display_name" ? 120
    : field.id === "email" ? 254
      : field.id === "phone" ? 24
        : field.id === "source" ? 40 : 4096;
  return Math.min(host, field.maxLength ?? host);
}

function effectiveTagCount(field: AppActionFieldV2): number {
  return Math.min(16, field.maxItems ?? 16);
}

function effectiveTagLength(field: AppActionFieldV2): number {
  return Math.min(40, field.maxLength ?? 40);
}

function parseTags(text: string, field: AppActionFieldV2): string[] {
  const tags = text.trim() === "" ? [] : text.split(/[\n,]/).map((tag) => tag.trim()).filter(Boolean);
  if (tags.length > effectiveTagCount(field)) throw new Error(`Use no more than ${effectiveTagCount(field)} tags.`);
  const maxLength = effectiveTagLength(field);
  if (tags.some((tag) => new TextEncoder().encode(tag).byteLength > maxLength)) {
    throw new Error(`Each tag must be at most ${maxLength} UTF-8 bytes.`);
  }
  return tags;
}

function parseActionReceipt(
  raw: unknown,
  receipt: AppViewReceipt,
  route: ResolvedLiveAction,
  installId: string,
  contextId: string,
  expectedRevision: number | null,
): string {
  const body = object(raw);
  if (!body || Object.keys(body).some((key) => !["record", "digest", "view_descriptor_digest", "view_binding_digest"].includes(key))) {
    throw new Error("The action returned an invalid host receipt.");
  }
  if (body.digest !== receipt.bundleDigest
      || body.view_descriptor_digest !== receipt.descriptorDigest
      || body.view_binding_digest !== receipt.bindingDigest) {
    throw new Error("The action receipt does not match the installed bundle, descriptor and binding pins.");
  }
  const row = object(body.record);
  if (!row || Object.keys(row).some((key) => !RECORD_KEYS.includes(key))) throw new Error("The action returned an unsupported record receipt.");
  const id = row.id;
  const revision = row.revision;
  if (!isSafeRecordId(id) || row.install_id !== installId || row.context_id !== contextId || row.kind !== "customer"
      || !Number.isSafeInteger(revision) || (revision as number) < 1 || !SHA256.test(String(row.digest))) {
    throw new Error("The action receipt does not match the trusted installation, context or customer identity.");
  }
  if (route.operation === "create") {
    if (!/^cust-[0-9a-f]{32}$/.test(id as string) || revision !== 1) throw new Error("The create receipt did not use a host-minted customer id.");
  } else if (id !== route.recordId || expectedRevision === null || revision !== expectedRevision + 1) {
    throw new Error("The update receipt does not match the expected customer revision.");
  }
  const profile = object(row.profile);
  if (!profile || Object.keys(profile).some((key) => !PROFILE_KEYS.includes(key))
      || profile.schema !== 1 || typeof profile.display_name !== "string" || !Array.isArray(profile.tags)
      || profile.tags.some((tag) => typeof tag !== "string") || !object(profile.consent)
      || !["unknown", "granted", "denied"].includes(String(object(profile.consent)!.email))) {
    throw new Error("The action returned an invalid customer profile receipt.");
  }
  if (!Array.isArray(row.history) || !Array.isArray(row.consent_history)) {
    throw new Error("The action returned an incomplete customer receipt.");
  }
  return id as string;
}

export default function LiveAppAction({
  installId,
  contextId,
  receipt,
  route,
  enabled,
  blockedReason,
  returnHref,
  detailHref,
}: {
  installId: string;
  contextId: string;
  receipt: AppViewReceipt;
  route: ResolvedLiveAction;
  enabled: boolean;
  blockedReason: string | null;
  returnHref: string;
  detailHref: (viewId: string, recordId: string) => string;
}) {
  const formId = useId();
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [readState, setReadState] = useState<ReadState | null>(null);
  const [readRetry, setReadRetry] = useState(0);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const mutation = useRef<AbortController | null>(null);
  const requestKey = useMemo(() => JSON.stringify([
    installId,
    contextId,
    route.action.id,
    route.recordId,
    receipt.bundleDigest,
    receipt.descriptorDigest,
    receipt.bindingDigest,
  ]), [installId, contextId, route, receipt]);

  useEffect(() => {
    if (!enabled) return;
    if (route.operation === "create") {
      setReadState({ key: requestKey, status: "success", revision: null, error: null });
      setDraft(EMPTY_DRAFT);
      return;
    }
    if (!route.detailRoute || !contextId) {
      setReadState({ key: requestKey, status: "error", revision: null, error: "The update route has no active context-bound detail read." });
      return;
    }
    const controller = new AbortController();
    setReadState({ key: requestKey, status: "loading", revision: null, error: null });
    workspaceApps.viewRead({
      installId,
      viewId: route.detailRoute.view.id,
      recordId: route.recordId ?? undefined,
      bundleDigest: receipt.bundleDigest,
      descriptorDigest: receipt.descriptorDigest!,
      bindingDigest: receipt.bindingDigest!,
      contextId,
      source: "customers",
    }, controller.signal)
      .then((raw) => {
        if (controller.signal.aborted) return;
        const result = detailDraft(raw, receipt, route);
        setDraft(result.draft);
        setReadState({ key: requestKey, status: "success", revision: result.revision, error: null });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          setReadState({
            key: requestKey,
            status: "error",
            revision: null,
            error: error instanceof Error ? error.message : "Could not load the current customer.",
          });
        }
      });
    return () => controller.abort();
  }, [enabled, installId, contextId, receipt, route, requestKey, readRetry]);

  useEffect(() => () => mutation.current?.abort(), [requestKey, enabled]);

  const current = readState?.key === requestKey ? readState : null;
  const ready = enabled && current?.status === "success" && !submitting;
  const fields = route.action.input.fields;
  const byId = (id: string) => fields.find((field) => field.id === id)!;

  if (!enabled) {
    return (
      <section className="av" data-live-action={route.action.id}>
        <Notice state="live-action-disabled">
          {blockedReason ?? "An active app context is required before customer actions are available."}{" "}
          <Link href={returnHref} className="lnk">Return to the installed app</Link>
        </Notice>
      </section>
    );
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!ready || !receipt.descriptorDigest || !receipt.bindingDigest
        || (mutation.current !== null && !mutation.current.signal.aborted)) return;
    setSubmitError(null);
    const tagsField = byId("tags");
    let tags: string[];
    try {
      tags = parseTags(draft.tags, tagsField);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : "Tags are invalid.");
      return;
    }
    const expectedRevision = route.operation === "update" ? current.revision : null;
    if (route.operation === "update" && expectedRevision === null) {
      setSubmitError("The host record revision is unavailable; reload the customer before saving.");
      return;
    }
    const input = {
      display_name: draft.display_name,
      email: draft.email.trim() === "" ? null : draft.email,
      phone: draft.phone.trim() === "" ? null : draft.phone,
      source: draft.source.trim() === "" ? null : draft.source,
      tags,
    };
    const controller = new AbortController();
    mutation.current?.abort();
    mutation.current = controller;
    setSubmitting(true);
    workspaceApps.viewAction({
      installId,
      contextId,
      viewId: route.action.form_view,
      actionId: route.action.id,
      ...(route.recordId === null ? {} : { recordId: route.recordId }),
      ...(expectedRevision === null ? {} : { expectedRevision }),
      bundleDigest: receipt.bundleDigest,
      descriptorDigest: receipt.descriptorDigest,
      bindingDigest: receipt.bindingDigest,
      input,
    }, controller.signal)
      .then((raw) => {
        if (controller.signal.aborted) return;
        const recordId = parseActionReceipt(raw, receipt, route, installId, contextId, expectedRevision);
        // The parent owns the trusted URL constructor; following the
        // receipt is the only success transition. No submit retry exists.
        navigate(detailHref(route.detailViewId, recordId), { replace: true });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          setSubmitError(error instanceof Error ? error.message : "The action was refused; your input is still here.");
        }
      })
      .finally(() => {
        if (mutation.current === controller) mutation.current = null;
        if (!controller.signal.aborted) setSubmitting(false);
      });
  };

  const renderTextField = (field: AppActionFieldV2) => {
    const value = draft[field.id as keyof Draft] as string;
    const clearHint = field.nullable
      ? route.operation === "update" ? "Leave blank to clear this field." : "Optional; leave blank for no value."
      : undefined;
    const maxLength = effectiveTextLimit(field);
    const inputType = field.id === "email" ? "email" : field.id === "phone" ? "tel" : "text";
    return (
      <Field
        key={field.id}
        id={`${formId}-${field.id}`}
        label={field.label}
        required={field.required}
        disabled={!ready}
        hint={clearHint}
        className="crm-field"
      >
        {(control) => (
          <input
            {...control}
            className="field"
            type={inputType}
            value={value}
            maxLength={maxLength}
            autoComplete="off"
            onChange={(event) => setDraft((previous) => ({ ...previous, [field.id]: event.target.value }))}
          />
        )}
      </Field>
    );
  };

  const tagsField = byId("tags");
  return (
    <section className="av" aria-labelledby={`${formId}-title`} data-live-action={route.action.id}>
      <header className="av-head">
        <h3 id={`${formId}-title`} className="text-cardtitle font-medium text-ink-100" tabIndex={-1}>
          {route.action.title}
        </h3>
        <p className="num text-micro text-ink-500">
          {receipt.actionDescriptor?.app} · live customer {route.operation} · operator-only
        </p>
      </header>
      <Notice state="live-action-pinned">
        This host-drawn form uses the verified app-actions/v2 companion. The v1 form preview remains inert; customer consent is not editable here.
      </Notice>
      <p><Link href={returnHref} className="lnk">← Cancel</Link></p>
      {current?.status === "loading" && <Loading>Reading the current customer and revision…</Loading>}
      {current?.status === "error" && (
        <ErrorNotice onRetry={() => setReadRetry((count) => count + 1)}>
          Could not load the customer for editing: {current.error}
        </ErrorNotice>
      )}
      {(route.operation === "create" || current?.status === "success") && (
        <form className="av-form" aria-label={route.action.title} onSubmit={submit}>
          <div className="crm-field-row">
            {fields.filter((field) => field.type === "text").map(renderTextField)}
          </div>
          <Field
            id={`${formId}-tags`}
            label={tagsField.label}
            disabled={!ready}
            hint={`Comma or newline separated; up to ${effectiveTagCount(tagsField)} tags of ${effectiveTagLength(tagsField)} bytes each.`}
            className="crm-field"
          >
            {(control) => (
              <textarea
                {...control}
                className="field"
                rows={3}
                maxLength={effectiveTagCount(tagsField) * (effectiveTagLength(tagsField) + 1)}
                value={draft.tags}
                onChange={(event) => setDraft((previous) => ({ ...previous, tags: event.target.value }))}
              />
            )}
          </Field>
          {submitError && <p className="text-label text-fail" role="alert">{submitError}</p>}
          <div className="av-form-actions">
            <button type="submit" className="btn btn-primary btn-sm" disabled={!ready}>
              {submitting ? "Saving…" : route.operation === "create" ? "Create customer" : "Save customer"}
            </button>
            <Link href={returnHref} className="btn btn-secondary btn-sm">Cancel</Link>
          </div>
        </form>
      )}
      {current?.revision !== null && current?.revision !== undefined && (
        <p className="num text-micro text-ink-500">Editing host record revision {current.revision}; the revision is not an app input.</p>
      )}
    </section>
  );
}
