import { useEffect, useId, useRef, useState } from "react";
import { useTranslate } from "../../../lib/locale";
import { onPrivateReadsPaused, withPrivateOperation } from "../../../lib/privateSession";
import { sessionHeaders } from "../../../lib/sessionKey";
import { readTeamMembers, type TeamMember } from "../../../lib/teamRoster";
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

type Permission = "read" | "write";
type Recipient = {
  kind: keyof typeof grantKinds;
  subject_id: string;
  permission: Permission;
};
type Grant = Recipient & { id: string };
type Change =
  | { kind: "revoke"; recipient: Grant }
  | { kind: "grant"; recipient: Recipient; label: string };
type SharingPage = {
  name: string | null;
  policy: { id: string; revision: number; visibility: "private" | "workspace" };
  grants: Grant[];
  next: string | null;
};
type Continuation = { policy_id: string; revision: number; cursor: string };
type State = { kind: "loading" | "unavailable" | "changed" | "error" | "outcome-unknown" } | { kind: "ready"; page: SharingPage };
type Review = { token: string; page: SharingPage; change: Change };

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
  phase: "inspect" | `${Change["kind"]}-${"prepare" | "confirm"}`,
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

function checkedReview(body: unknown, recordId: string, page: SharingPage, change: Change): Review | null {
  if (!object(body) || !exact(body, ["confirmation_token", "record", "policy", "grant"]) ||
      typeof body.confirmation_token !== "string" || !/^[a-f0-9]{64}$/.test(body.confirmation_token)) return null;
  if (!object(body.grant) || !exact(body.grant, change.kind === "revoke"
    ? ["id", "kind", "subject_id", "permission"] : ["kind", "subject_id", "permission"])) return null;
  const shown = sharingPage({ record: body.record, policy: body.policy, grants: [], next_cursor: null }, recordId);
  if (!shown || shown.policy.id !== page.policy.id || shown.policy.revision !== page.policy.revision ||
      shown.policy.visibility !== page.policy.visibility) return null;
  const grant = body.grant;
  if ((change.kind === "revoke" && grant.id !== change.recipient.id) ||
      grant.kind !== change.recipient.kind || grant.subject_id !== change.recipient.subject_id ||
      grant.permission !== change.recipient.permission) return null;
  return { token: body.confirmation_token, page: shown, change };
}

function AddAccess({ busy, onReview, onClose }: {
  busy: boolean;
  onReview: (member: TeamMember, permission: Permission, button: HTMLElement) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const id = useId();
  const [members, setMembers] = useState<TeamMember[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [memberId, setMemberId] = useState("");
  const [permission, setPermission] = useState<Permission>("read");
  const select = useRef<HTMLSelectElement>(null);
  const form = useRef<HTMLFormElement>(null);
  useEffect(() => {
    const controller = new AbortController();
    void readTeamMembers(controller.signal).then((rows) => {
      if (!controller.signal.aborted) setMembers(rows);
    }, () => {
      if (!controller.signal.aborted) setFailed(true);
    });
    return () => controller.abort();
  }, [attempt]);
  useEffect(() => {
    form.current?.querySelector<HTMLButtonElement>("button[type=button]")?.focus();
  }, []);
  useEffect(() => {
    if (members?.length) select.current?.focus();
  }, [members]);
  const duplicates = new Map<string, number>();
  const nameOf = (member: TeamMember) => member.name.trim() || member.userId;
  for (const member of members ?? []) {
    const name = nameOf(member);
    duplicates.set(name, (duplicates.get(name) ?? 0) + 1);
  }
  return (
    <form ref={form} className="wa-sharing-form" aria-label={t("Add or edit access")} onSubmit={(event) => {
      event.preventDefault();
      const member = members?.find((row) => row.userId === memberId);
      const button = event.currentTarget.querySelector<HTMLButtonElement>("button[type=submit]");
      if (member && button) onReview(member, permission, button);
    }}>
      <p>{t("Choose a workspace member. Other recipient types aren’t available here.")}</p>
      {members === null && !failed && <p role="status">{t("Loading workspace members…")}</p>}
      {failed && <div className="wa-customer-sharing-state">
        <p role="status">{t("Workspace members could not be loaded.")}</p>
        <Button onClick={() => { setFailed(false); setAttempt((value) => value + 1); }}>{t("Retry")}</Button>
      </div>}
      {members?.length === 0 && <p role="status">{t("No workspace members are available.")}</p>}
      {members && members.length > 0 && <>
        <label htmlFor={`${id}-member`}>{t("Access recipient")}</label>
        <select id={`${id}-member`} ref={select} className="field" required disabled={busy}
          value={memberId} onChange={(event) => setMemberId(event.target.value)}>
          <option value="" disabled>{t("Choose a workspace member")}</option>
          {members.map((member) => (
            <option key={member.userId} value={member.userId}>
              {nameOf(member)}{(duplicates.get(nameOf(member)) ?? 0) > 1 ? ` — ${member.email || member.userId}` : ""}
            </option>
          ))}
        </select>
        <label htmlFor={`${id}-permission`}>{t("Permission")}</label>
        <select id={`${id}-permission`} className="field" disabled={busy}
          value={permission} onChange={(event) => setPermission(event.target.value === "write" ? "write" : "read")}>
          <option value="read">{t("Can view")}</option>
          <option value="write">{t("Can edit")}</option>
        </select>
        <Button type="submit" variant="primary" loading={busy}>{t("Review change")}</Button>
      </>}
      <Button disabled={busy} onClick={onClose}>{t("Back")}</Button>
    </form>
  );
}

function SharingDialog({ actionToken, recordId, onClose }: { actionToken: string; recordId: string; onClose: () => void }) {
  const t = useTranslate();
  const [request, setRequest] = useState<{ page?: Continuation }>({});
  const [state, setState] = useState<State>({ kind: "loading" });
  const [review, setReview] = useState<Review | null>(null);
  const [busy, setBusy] = useState(false);
  const [revokeUnavailable, setRevokeUnavailable] = useState(false);
  const [editing, setEditing] = useState(false);
  const [grantError, setGrantError] = useState<"target" | "unsupported" | null>(null);
  const addTrigger = useRef<HTMLButtonElement>(null);
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
    setEditing(false);
    setGrantError(null);
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
  const prepare = (page: SharingPage, change: Change, button: HTMLElement) => perform(async (signal) => {
    setGrantError(null);
    const result = await sharingRequest(`${change.kind}-prepare`, actionToken, recordId, {
      policy_id: page.policy.id, revision: page.policy.revision,
      ...(change.kind === "revoke" ? { grant_id: change.recipient.id } : {
        target: { kind: change.recipient.kind, id: change.recipient.subject_id },
        permission: change.recipient.permission,
      }),
    }, signal).catch(() => null);
    if (signal.aborted) return;
    const card = result?.ok ? checkedReview(result.body, recordId, page, change) : null;
    if (card) {
      opener.current = button;
      setReview(card);
    } else if (result?.status === 409 && object(result.body) && result.body.code === "sharing_changed") {
      setState({ kind: "changed" });
    } else if (result?.status === 401 || result?.status === 403 ||
        (result?.status === 404 && object(result.body) && result.body.code === "not_found")) {
      setState({ kind: "unavailable" });
    } else {
      if (change.kind === "revoke") setRevokeUnavailable(true);
      else setGrantError(result?.status === 409 && object(result.body) && result.body.code === "sharing_target_unavailable"
        ? "target" : "unsupported");
    }
  });
  const decide = (card: Review, confirmed: boolean) => perform(async (signal) => {
    const result = await sharingRequest(`${card.change.kind}-confirm`, actionToken, recordId, {
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
    } else if (card.change.kind === "grant" && result?.status === 409 &&
        object(result.body) && result.body.code === "sharing_target_unavailable") {
      setGrantError("target");
      opener.current = null;
      setEditing(false);
      requestAnimationFrame(() => addTrigger.current?.focus());
    } else {
      setState({ kind: result?.status === 409 ? "changed" : "outcome-unknown" });
    }
  });
  const content = state.kind === "ready" ? state.page : null;
  const next = content?.next;
  const reviewTitle = t(review?.change.kind === "grant" ? "Review access change" : "Review access removal");
  const closeEditor = () => {
    setEditing(false);
    setGrantError(null);
    requestAnimationFrame(() => addTrigger.current?.focus());
  };
  return (
    <WorkspaceDialog title={t("Customer sharing")} className="wa-customer-sharing" onClose={() => {
      if (busy) return;
      if (review) void decide(review, false);
      else if (editing) closeEditor();
      else onClose();
    }}>
      {state.kind === "loading" && <p role="status">{t("Loading sharing settings…")}</p>}
      {state.kind === "unavailable" && <p role="status">{t("Sharing settings are unavailable for this customer.")}</p>}
      {(state.kind === "error" || state.kind === "changed" || state.kind === "outcome-unknown") && <div className="wa-customer-sharing-state">
        <p role="status">{t(state.kind === "changed" ? "Sharing changed. Reload to continue." :
          state.kind === "outcome-unknown" ? "Could not verify the access change. Reload sharing to check." : "Could not load sharing settings.")}</p>
        <Button onClick={() => load()}>{t("Reload sharing")}</Button>
      </div>}
      {review && <section aria-label={reviewTitle}>
        <h3>{reviewTitle}</h3>
        {review.change.kind === "revoke" && <p>{t("Other grants or workspace visibility may still allow access.")}</p>}
        <dl className="wa-customer-sharing-summary">
          <div><dt>{t("Customer")}</dt><dd>{review.page.name || recordId}</dd></div>
          <div><dt>{t("Access recipient")}</dt><dd>{t(grantKinds[review.change.recipient.kind])} · {review.change.kind === "grant" && <>{review.change.label} · </>}<bdi>{review.change.recipient.subject_id}</bdi></dd></div>
          <div><dt>{t(review.change.kind === "grant" ? "Permission" : "Permission to remove")}</dt><dd>{t(review.change.recipient.permission === "read" ? "Can view" : "Can edit")}</dd></div>
          <div><dt>{t("Revision")}</dt><dd>{review.page.policy.revision}</dd></div>
        </dl>
        <div className="wa-customer-sharing-state" ref={reviewActions}>
          <Button disabled={busy} onClick={() => void decide(review, false)}>{t("Cancel")}</Button>
          <Button variant={review.change.kind === "grant" ? "primary" : "danger"} loading={busy} onClick={() => void decide(review, true)}>
            {t(review.change.kind === "grant" ? "Confirm access change" : "Confirm removal")}
          </Button>
        </div>
      </section>}
      {content && <div hidden={review !== null}>
        <dl className="wa-customer-sharing-summary">
          <div><dt>{t("Customer")}</dt><dd>{content.name || recordId}</dd></div>
          <div><dt>{t("Visibility")}</dt><dd>{t(content.policy.visibility === "private" ? "Private customer" : "Workspace-visible")}</dd></div>
          <div><dt>{t("Revision")}</dt><dd>{content.policy.revision}</dd></div>
        </dl>
        {grantError !== null && <p role="status">{t(grantError === "target"
          ? "This recipient could not be verified. Check the member list and try again."
          : "Adding or editing access is unavailable on this host.")}</p>}
        {editing ? <AddAccess busy={busy} onClose={closeEditor} onReview={(member, permission, button) =>
          void prepare(content, { kind: "grant", label: member.name.trim() || member.userId,
            recipient: { kind: "human", subject_id: member.userId, permission } }, button)}
        /> : <button type="button" ref={addTrigger} className="btn btn-secondary" disabled={busy}
          onClick={() => { setGrantError(null); setEditing(true); }}>{t("Add or edit access")}</button>}
        <h3>{t("Access grants")}</h3>
        {content.grants.length === 0 ? <p role="status">{t("No access grants on this page.")}</p> : <ul className="wa-customer-sharing-grants">
          {content.grants.map(grant => <li key={grant.id}>
            <div>{t(grantKinds[grant.kind])}<bdi>{grant.subject_id}</bdi></div>
            <span>{t(grant.permission === "read" ? "Can view" : "Can edit")}</span>
            {!revokeUnavailable && <Button disabled={busy || editing} onClick={(event) => void prepare(content, { kind: "revoke", recipient: grant }, event.currentTarget)}>{t("Revoke access")}</Button>}
          </li>)}
        </ul>}
        <div className="wa-customer-sharing-state">
          {request.page && <Button disabled={busy || editing} onClick={() => load()}>{t("First page")}</Button>}
          {next && <Button disabled={busy || editing} onClick={() => load({ policy_id: content.policy.id, revision: content.policy.revision, cursor: next })}>{t("Next grants")}</Button>}
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
