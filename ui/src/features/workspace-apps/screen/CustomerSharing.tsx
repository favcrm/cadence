import { useEffect, useState } from "react";
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
type State = { kind: "loading" | "unavailable" | "changed" | "error" } | { kind: "ready"; page: SharingPage };

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

async function inspect(actionToken: string, recordId: string, page: Continuation | undefined, signal: AbortSignal): Promise<State> {
  return withPrivateOperation(async (operation) => {
    const response = await operation.fetch("/api/app-screen-sharing/inspect", {
      method: "POST", credentials: "same-origin", cache: "no-store", redirect: "error",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ action_token: actionToken, record_id: recordId, ...(page ? { page } : {}) }),
    });
    const body: unknown = await response.json().catch(() => null);
    if (!response.ok) {
      if (response.status === 409 && object(body) && body.code === "sharing_changed") return { kind: "changed" };
      if (response.status >= 500) return { kind: "error" };
      return { kind: "unavailable" };
    }
    const result = sharingPage(body, recordId, page);
    return result ? { kind: "ready", page: result } : { kind: "unavailable" };
  }, signal);
}

function SharingDialog({ actionToken, recordId, onClose }: { actionToken: string; recordId: string; onClose: () => void }) {
  const t = useTranslate();
  const [request, setRequest] = useState<{ page?: Continuation }>({});
  const [state, setState] = useState<State>({ kind: "loading" });
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
  const content = state.kind === "ready" ? state.page : null;
  const next = content?.next;
  return (
    <WorkspaceDialog title={t("Customer sharing")} className="wa-customer-sharing" onClose={onClose}>
      {state.kind === "loading" && <p role="status">{t("Loading sharing settings…")}</p>}
      {state.kind === "unavailable" && <p role="status">{t("Sharing settings are unavailable for this customer.")}</p>}
      {(state.kind === "error" || state.kind === "changed") && <div className="wa-customer-sharing-state">
        <p role="status">{t(state.kind === "changed" ? "Sharing changed. Reload to continue." : "Could not load sharing settings.")}</p>
        <Button onClick={() => load()}>{t("Reload sharing")}</Button>
      </div>}
      {content && <>
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
          </li>)}
        </ul>}
        <div className="wa-customer-sharing-state">
          {request.page && <Button onClick={() => load()}>{t("First page")}</Button>}
          {next && <Button onClick={() => load({ policy_id: content.policy.id, revision: content.policy.revision, cursor: next })}>{t("Next grants")}</Button>}
        </div>
        <p className="wa-customer-sharing-note">{t("Review only. Sharing changes aren’t available here yet.")}</p>
      </>}
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
