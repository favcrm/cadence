import { useEffect, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { declaredSlots, isCatalogSlot, type DeclaredSlot } from "./bindingChoices";
import { parseCatalogGrant, type CatalogEffect, type CatalogTool } from "./platformCatalog";
import { completeRequest, retainedRequest } from "./requests";
import { workspaceApps, type AppBinding, type Installation, type PlatformToolRequest } from "./workspaceApps";

type Props = {
  installation: Installation;
  bindings: AppBinding[];
  canWrite: boolean;
  busy: boolean;
  mutate: (work: () => Promise<void>) => Promise<void>;
  refresh: () => void;
};

/** Owner-selected catalogue grants are separate from provider connections. */
export function CatalogBindings(props: Props) {
  const t = useTranslate();
  const [requests, setRequests] = useState<PlatformToolRequest[] | null | undefined>(undefined);
  const [requestError, setRequestError] = useState<"retry" | "refresh" | null>(null);
  const [retry, setRetry] = useState(0);
  const { refresh } = props;
  const slots = props.installation.source_kind === "private"
    ? declaredSlots(props.installation).filter(isCatalogSlot)
    : [];
  useEffect(() => {
    if (!props.canWrite || slots.length === 0) return;
    const controller = new AbortController();
    setRequests(undefined); setRequestError(null);
    void workspaceApps.platformToolRequests(props.installation.install_id, props.installation.digest, controller.signal).then(
      value => { if (!controller.signal.aborted) setRequests(value); },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && error.code === "installation_stale") {
          if (retry > 0) refresh();
          else setRequestError("refresh");
        } else setRequestError("retry");
      },
    );
    return () => controller.abort();
  }, [props.installation.install_id, props.installation.digest, props.canWrite, slots.length, retry, refresh]);
  if (slots.length === 0) return null;
  return <>
    {props.canWrite && (requestError
      ? <div className="card mcard"><p className="wa-alert" role="alert">{t("Could not load tool access requests.")}</p><Button disabled={props.busy} onClick={requestError === "refresh" ? refresh : () => setRetry(n => n + 1)}>{t("Retry")}</Button></div>
      : requests === undefined ? <p role="status">{t("Loading tool access requests…")}</p>
      : requests !== null && requests.length === 0 ? <p className="wa-muted">{t("No pending tool access requests.")}</p> : null)}
    {slots.map(slot => <CatalogSlot key={slot.slot} {...props} slot={slot}
      requests={requests?.filter(request => request.slot === slot.slot &&
        request.install_id === props.installation.install_id && request.bundle_digest === props.installation.digest) ?? []} />)}
  </>;
}

function CatalogSlot({ installation, bindings, canWrite, busy, mutate, slot, requests }: Props & { slot: DeclaredSlot; requests: PlatformToolRequest[] }) {
  const t = useTranslate();
  const [tools, setTools] = useState<CatalogTool[] | null>(null);
  const [selected, setSelected] = useState("");
  const [accountId, setAccountId] = useState("");
  const [unavailable, setUnavailable] = useState(false);
  const effect: CatalogEffect = slot.declaration?.effect === "draft" ? "draft" : "read";
  const rows = bindings.filter(row => row.slot === slot.slot && row.context_id === null && row.state === "configured");
  let binding: AppBinding | null = null;
  let invalid = rows.length > 1;
  if (rows.length === 1) {
    const candidate = rows[0];
    const slug = candidate.config?.tool_slug;
    if (typeof slug !== "string") invalid = true;
    else {
      try {
        parseCatalogGrant({ binding: candidate }, { installId: installation.install_id, digest: installation.digest, slot: slot.slot, slug, effect,
          ...(typeof candidate.config.connection_id === "string" ? { connectionId: candidate.config.connection_id } : {}) });
        binding = candidate;
      } catch { invalid = true; }
    }
  }
  const bindingId = binding?.id ?? null;
  const loadTools = bindingId === null || requests.length > 0;
  useEffect(() => {
    setSelected(""); setAccountId(""); setTools(null); setUnavailable(false);
    if (!canWrite || invalid || !loadTools) return;
    const controller = new AbortController();
    void workspaceApps.platformTools(installation.install_id, slot.slot, effect, controller.signal).then(
      value => { if (!controller.signal.aborted) setTools(value); },
      () => { if (!controller.signal.aborted) setUnavailable(true); },
    );
    return () => controller.abort();
  }, [installation.install_id, installation.digest, slot.slot, effect, canWrite, bindingId, invalid, loadTools]);
  const selectedTool = tools?.find(tool => tool.slug === selected);
  const needsAccount = selectedTool?.connectedAccounts !== undefined;
  const selectedAccount = selectedTool?.connectedAccounts?.find(account => account.id === accountId);
  return <section className="card mcard wa-stack" aria-label={`${slot.slot} ${t("platform tool")}`}>
    <h3>{t(effect === "read" ? "Read tool" : "Draft tool")} <span className="text-ink-400 text-label">· {slot.slot}</span></h3>
    <p className="wa-muted">{t("The app can send inputs to this tool. Charges use the workspace balance at the price when the tool runs.")}</p>
    <p className="wa-muted">{t("Some app screens run tools as the owner without asking again.")}</p>
    {canWrite && requests.map(request => {
      const label = tools?.find(tool => tool.slug === request.tool_slug)?.displayName;
      return <div key={request.id} className="wa-stack">
        <p className="wa-kicker break-words">{t("Requested tool")}: {label && label !== request.tool_slug
          ? <>{label} <code className="text-label">({request.tool_slug})</code></>
          : <code>{request.tool_slug}</code>}</p>
        <div className="flex flex-wrap gap-2">
          <Button variant="primary" disabled={busy || invalid || binding !== null} onClick={() => void mutate(async () => {
            await workspaceApps.decidePlatformToolRequest(request, "approve");
          })}>{t("Grant access")}</Button>
          <Button disabled={busy} onClick={() => void mutate(async () => {
            await workspaceApps.decidePlatformToolRequest(request, "deny");
          })}>{t("Decline")}</Button>
        </div>
      </div>;
    })}
    {invalid ? <p className="wa-alert" role="alert">{t("The saved tool grant could not be verified.")}</p> : binding ? <>
      <p className="wa-kicker break-words">{t("Granted tool")}: {binding.config.tool_slug?.replace(/[_.-]+/g, " ").replace(/^./, (letter) => letter.toUpperCase())}</p>
      <details className="text-label">
        <summary className="cursor-pointer">{t("Technical details")}</summary>
        <p className="break-all">{t("Tool")}: <code>{binding.config.tool_slug}</code></p>
        {binding.config.connection_id && <p className="break-all">{t("Connected account")}: <code>{binding.config.connection_id}</code></p>}
      </details>
      <Button disabled={!canWrite || busy} loading={busy} onClick={() => void mutate(async () => {
        const slug = binding.config.tool_slug;
        if (!slug) throw new Error(t("The tool grant is unavailable."));
        await workspaceApps.revokePlatformTool(installation.install_id, binding.id, binding.revision, { digest: installation.digest, slot: slot.slot, slug, effect,
          ...(binding.config.connection_id ? { connectionId: binding.config.connection_id } : {}) });
      })}>{t("Remove tool access")}</Button>
    </> : !canWrite ? <p className="wa-muted">{t("Only an owner can grant a platform tool.")}</p> : unavailable ?
      <p className="wa-alert" role="alert">{t("The platform tool catalogue is unavailable.")}</p> : tools === null ?
        <p role="status">{t("Loading platform tools…")}</p> : tools.length === 0 ?
          <p className="wa-muted">{t("No matching platform tools are available.")}</p> : <>
            <p className="wa-muted">{t("Choose what this app may use. Granting access does not run the tool.")}</p>
            <p className="wa-muted">{t("Listed prices are estimates.")}</p>
            <Select value={selected} onChange={value => { setSelected(value); setAccountId(""); }} disabled={busy} full
              aria-label={t("Choose platform tool")}
              placeholder={t("Choose platform tool")}
              options={tools.map(tool => ({ value: tool.slug, label: tool.displayName,
                hint: `${tool.price.currency} ${tool.price.amount} ${t("per call")}${tool.unitPrice ? ` · ${tool.unitPrice.currency} ${tool.unitPrice.amount} ${t("per unit")}` : ""}` }))} />
            {needsAccount && (selectedTool?.connectedAccounts?.length
              ? <Select value={accountId} onChange={setAccountId} disabled={busy} full
                  aria-label={t("Choose connected account")} placeholder={t("Choose connected account")}
                  options={selectedTool.connectedAccounts.map(account => ({ value: account.id, label: account.account || account.provider, hint: account.id }))} />
              : <p className="wa-alert">{t("No eligible connected account is available for this tool.")}</p>)}
            <Button variant="primary" disabled={busy || !selectedTool || (needsAccount && !selectedAccount)} loading={busy} onClick={() => void mutate(async () => {
              if (!selectedTool || (needsAccount && !selectedAccount)) return;
              const key = JSON.stringify([installation.install_id, "catalog", installation.digest, slot.slot, selectedTool.slug, selectedAccount?.id ?? null]);
              const receipt = await workspaceApps.bindPlatformTool(installation.install_id, { slot: slot.slot, tool_slug: selectedTool.slug, expected_digest: installation.digest, request_id: retainedRequest(key),
                ...(selectedAccount ? { connection_id: selectedAccount.id } : {}) }, effect);
              completeRequest(key);
              if (receipt.state !== "configured") throw new Error(t("This tool grant was revoked. Choose it again."));
              setSelected("");
            })}>{t("Grant tool access")}</Button>
          </>}
  </section>;
}
