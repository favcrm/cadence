import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../../lib/locale";
import { onPrivateReadsPaused, withPrivateOperation } from "../../../lib/privateSession";
import { sessionHeaders } from "../../../lib/sessionKey";
import Button from "../../../ui/Button";
import { WorkspaceDialog } from "../WorkspaceDialog";
import { exact, object } from "./screenServices";

const grantKinds = {
  human: "Person",
  team: "Team",
  agent: "Agent",
  integration: "Integration",
} as const;
const handle = (value: unknown): value is string =>
  typeof value === "string" && /^[a-f0-9]{32}$/.test(value);

type Grant = {
  id: string;
  kind: keyof typeof grantKinds;
  subject_id: string;
  permission: "read" | "write";
};
type SharingPage = {
  name: string | null;
  policy: { id: string; revision: number; visibility: "private" | "workspace" };
  grants: Grant[];
  next: string | null;
};
type Continuation = { policy_id: string; revision: number; cursor: string };
type State = { kind: "loading" | "unavailable" | "changed" | "error" | "revoke-unknown" } | { kind: "ready"; page: SharingPage };
type Review = { token: string; page: SharingPage; grant: Grant };

function sharingPage(value: unknown, recordId: string, after?: Continuation): SharingPage | null {
  if (!object(value) || !exact(value, ["record", "policy", "grants", "next_cursor"])) return null;
  const { record, policy, grants, next_cursor: next } = value;
  if (!object(record) || !exact(record, ["id", "display_name"]) || record.id !== recordId ||
      (record.display_name !== null && typeof record.display_name !== "string") ||
      !object(policy) || !exact(policy, ["id", "revision", "visibility"]) || !handle(policy.id) ||
      typeof policy.revision !== "number" || !Number.isSafeInteger(policy.revision) || policy.revision < 1 ||
      (policy.visibility !== "private" && policy.visibility !== "workspace") ||
      (after && (policy.id !== after.policy_id || policy.revision !== after.revision)) ||
      !Array.isArray(grants) || grants.length > 25 || (next !== null && !handle(next))) return null;
  const rows: Grant[] = [];
  let previous = after?.cursor ?? "";
  for (const row of grants) {
    if (!object(row) || !exact(row, ["id", "kind", "subject_id", "permission"]) ||
        !handle(row.id) || row.id <= previous ||
        (row.kind !== "human" && row.kind !== "team" && row.kind !== "agent" && row.kind !== "integration") ||
        typeof row.subject_id !== "string" || row.subject_id.length === 0 || row.subject_id.length > 200 ||
        (row.permission !== "read" && row.permission !== "write")) return null;
    rows.push({ id: row.id, kind: row.kind, subject_id: row.subject_id, permission: row.permission });
    previous = row.id;
  }
  if (next !== null && (rows.length === 0 || next !== previous)) return null;
  return {
    name: record.display_name,
    policy: { id: policy.id, revision: policy.revision, visibility: policy.visibility },
    grants: rows,
    next,
  };
}

function sharingRequest(
  phase: "inspect" | "revoke-prepare" | "revoke-confirm",
  actionToken: string,
  recordId: string,
  fields: Record<string, unknown>,
  signal: AbortSignal,
) {
  return withPrivateOperation(async (operation) => {
    const response = await operation.fetch(`/api/app-screen-sharing/${phase}`, {
      method: "POST", credentials: "same-origin", cache: "no-store", redirect: "error",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ action_token: actionToken, record_id: recordId, ...fields }),
    });
    const body: unknown = await response.json().catch(() => null);
    return { status: response.status, ok: response.ok, body };
  }, signal);
}

async function inspect(actionToken: string, recordId: string, page: Continuation | undefined, signal: AbortSignal): Promise<State> {
  const response = await sharingRequest("inspect", actionToken, recordId, page ? { page } : {}, signal);
  const { body } = response;
  if (!response.ok) {
    if (response.status === 409 && object(body) && body.code === "sharing_changed") return { kind: "changed" };
    if (response.status >= 500) return { kind: "error" };
    return { kind: "unavailable" };
  }
  const result = sharingPage(body, recordId, page);
  return result ? { kind: "ready", page: result } : { kind: "unavailable" };
}

function checkedReview(body: unknown, recordId: string, page: SharingPage, selected: Grant): Review | null {
  if (!object(body) || !exact(body, ["confirmation_token", "record", "policy", "grant"]) ||
      typeof body.confirmation_token !== "string" || !/^[a-f0-9]{64}$/.test(body.confirmation_token)) return null;
  const shown = sharingPage({ record: body.record, policy: body.policy, grants: [body.grant], next_cursor: null }, recordId);
  if (!shown || shown.policy.id !== page.policy.id || shown.policy.revision !== page.policy.revision ||
      shown.policy.visibility !== page.policy.visibility) return null;
  const grant = shown.grants[0];
  if (grant.id !== selected.id || grant.kind !== selected.kind || grant.subject_id !== selected.subject_id ||
      grant.permission !== selected.permission) return null;
  return { token: body.confirmation_token, page: shown, grant };
}

function SharingDialog({ actionToken, recordId, onClose }: { actionToken: string; recordId: string; onClose: () => void }) {
  const t = useTranslate();
  const [request, setRequest] = useState<{ page?: Continuation }>({});
  const [state, setState] = useState<State>({ kind: "loading" });
  const [review, setReview] = useState<Review | null>(null);
  const [busy, setBusy] = useState(false);
  const [revokeUnavailable, setRevokeUnavailable] = useState(false);
  const pending = useRef<AbortController | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const reviewActions = useRef<HTMLDivElement>(null);
  useEffect(() => () => pending.current?.abort(), []);
  useEffect(() => {
    if (review) reviewActions.current?.querySelector("button")?.focus();
    else if (opener.current) {
      opener.current.focus();
      opener.current = null;
    }
  }, [review]);
  useEffect(() => {
    const controller = new AbortController();
    void inspect(actionToken, recordId, request.page, controller.signal).then((result) => {
      if (!controller.signal.aborted) setState(result);
    }, () => {
      if (!controller.signal.aborted) setState({ kind: "error" });
    });
    return () => controller.abort();
  }, [actionToken, recordId, request]);
  const load = (page?: Continuation) => {
    setState({ kind: "loading" });
    setRequest({ page });
  };
  const perform = async (run: (signal: AbortSignal) => Promise<void>) => {
    if (pending.current) return;
    const controller = new AbortController();
    pending.current = controller;
    setBusy(true);
    try {
      await run(controller.signal);
    } finally {
      if (!controller.signal.aborted) {
        pending.current = null;
        setBusy(false);
      }
    }
  };
  const prepare = (page: SharingPage, grant: Grant, button: HTMLElement) => perform(async (signal) => {
    const result = await sharingRequest("revoke-prepare", actionToken, recordId, {
      policy_id: page.policy.id, revision: page.policy.revision, grant_id: grant.id,
    }, signal).catch(() => null);
    if (signal.aborted) return;
    const card = result?.ok ? checkedReview(result.body, recordId, page, grant) : null;
    if (card) {
      opener.current = button;
      setReview(card);
    } else if (result?.status === 409 && object(result.body) && result.body.code === "sharing_changed") {
      setState({ kind: "changed" });
    } else if (result?.status === 401 || result?.status === 403 ||
        (result?.status === 404 && object(result.body) && result.body.code === "not_found")) {
      setState({ kind: "unavailable" });
    } else {
      setRevokeUnavailable(true);
    }
  });
  const decide = (card: Review, confirmed: boolean) => perform(async (signal) => {
    const result = await sharingRequest("revoke-confirm", actionToken, recordId, {
      confirmation_token: card.token, confirmed,
    }, signal).catch(() => null);
    if (signal.aborted) return;
    setReview(null);
    if (!confirmed) return;
    opener.current = null;
    if (result?.ok && object(result.body) && exact(result.body, ["policy"]) &&
        object(result.body.policy) && exact(result.body.policy, ["id", "revision"]) &&
        result.body.policy.id === card.page.policy.id && result.body.policy.revision === card.page.policy.revision + 1) {
      load();
    } else {
      setState({ kind: result?.status === 409 ? "changed" : "revoke-unknown" });
    }
  });
  const content = state.kind === "ready" ? state.page : null;
  const next = content?.next;
  return (
    <WorkspaceDialog title={t("Customer sharing")} className="wa-customer-sharing" onClose={() => {
      if (review && !busy) void decide(review, false);
      else onClose();
    }}>
      {state.kind === "loading" && <p role="status">{t("Loading sharing settings…")}</p>}
      {state.kind === "unavailable" && <p role="status">{t("Sharing settings are unavailable for this customer.")}</p>}
      {(state.kind === "error" || state.kind === "changed" || state.kind === "revoke-unknown") && <div className="wa-customer-sharing-state">
        <p role="status">{t(state.kind === "changed" ? "Sharing changed. Reload to continue." :
          state.kind === "revoke-unknown" ? "Could not verify whether access was revoked. Reload sharing to check." : "Could not load sharing settings.")}</p>
        <Button onClick={() => load()}>{t("Reload sharing")}</Button>
      </div>}
      {review && <section aria-label={t("Review access removal")}>
        <h3>{t("Review access removal")}</h3>
        <p>{t("Other grants or workspace visibility may still allow access.")}</p>
        <dl className="wa-customer-sharing-summary">
          <div><dt>{t("Customer")}</dt><dd>{review.page.name || recordId}</dd></div>
          <div><dt>{t("Access grant")}</dt><dd>{t(grantKinds[review.grant.kind])} · <bdi>{review.grant.subject_id}</bdi></dd></div>
          <div><dt>{t("Permission to remove")}</dt><dd>{t(review.grant.permission === "read" ? "Can view" : "Can edit")}</dd></div>
          <div><dt>{t("Revision")}</dt><dd>{review.page.policy.revision}</dd></div>
        </dl>
        <div className="wa-customer-sharing-state" ref={reviewActions}>
          <Button disabled={busy} onClick={() => void decide(review, false)}>{t("Cancel")}</Button>
          <Button variant="danger" loading={busy} onClick={() => void decide(review, true)}>{t("Confirm removal")}</Button>
        </div>
      </section>}
      {content && <div hidden={review !== null}>
        <dl className="wa-customer-sharing-summary">
          <div><dt>{t("Customer")}</dt><dd>{content.name || recordId}</dd></div>
          <div><dt>{t("Visibility")}</dt><dd>{t(content.policy.visibility === "private" ? "Private customer" : "Workspace-visible")}</dd></div>
          <div><dt>{t("Revision")}</dt><dd>{content.policy.revision}</dd></div>
        </dl>
        <h3>{t("Access grants")}</h3>
        {content.grants.length === 0 ? <p role="status">{t("No access grants on this page.")}</p> : <ul className="wa-customer-sharing-grants">
          {content.grants.map(grant => <li key={grant.id}>
            <div>{t(grantKinds[grant.kind])}<bdi>{grant.subject_id}</bdi></div>
            <span>{t(grant.permission === "read" ? "Can view" : "Can edit")}</span>
            {!revokeUnavailable && <Button disabled={busy} onClick={(event) => void prepare(content, grant, event.currentTarget)}>{t("Revoke access")}</Button>}
          </li>)}
        </ul>}
        <div className="wa-customer-sharing-state">
          {request.page && <Button disabled={busy} onClick={() => load()}>{t("First page")}</Button>}
          {next && <Button disabled={busy} onClick={() => load({ policy_id: content.policy.id, revision: content.policy.revision, cursor: next })}>{t("Next grants")}</Button>}
        </div>
        {revokeUnavailable && <p className="wa-customer-sharing-note" role="status">{t("Removing access is unavailable. You can still view existing grants.")}</p>}
      </div>}
    </WorkspaceDialog>
  );
}

/** The caller keys this control to a ready primary mount and selected customer. */
export default function CustomerSharing({ actionToken, recordId }: { actionToken: string; recordId: string }) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  useEffect(() => onPrivateReadsPaused(() => setOpen(false)), []);
  return <>
    <div className="wa-customer-sharing-trigger">
      <Button onClick={() => setOpen(true)}>{t("Customer sharing")}</Button>
    </div>
    {open && <SharingDialog actionToken={actionToken} recordId={recordId} onClose={() => setOpen(false)} />}
  </>;
}
