import { useEffect, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useTranslate } from "../../lib/locale";
import { declaredSlots, isCatalogSlot, type DeclaredSlot } from "./bindingChoices";
import { parseCatalogGrant, type CatalogEffect, type CatalogTool } from "./platformCatalog";
import { completeRequest, retainedRequest } from "./requests";
import { workspaceApps, type AppBinding, type Installation } from "./workspaceApps";

type Props = {
  installation: Installation;
  bindings: AppBinding[];
  canWrite: boolean;
  busy: boolean;
  mutate: (work: () => Promise<void>) => Promise<void>;
};

/** Owner-selected catalogue grants are separate from provider connections. */
export function CatalogBindings(props: Props) {
  const slots = props.installation.source_kind === "private"
    ? declaredSlots(props.installation).filter(isCatalogSlot)
    : [];
  if (slots.length === 0) return null;
  return <>{slots.map(slot => <CatalogSlot key={slot.slot} {...props} slot={slot} />)}</>;
}

function CatalogSlot({ installation, bindings, canWrite, busy, mutate, slot }: Props & { slot: DeclaredSlot }) {
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
  useEffect(() => {
    setSelected(""); setAccountId(""); setTools(null); setUnavailable(false);
    if (!canWrite || bindingId || invalid) return;
    const controller = new AbortController();
    void workspaceApps.platformTools(installation.install_id, slot.slot, effect, controller.signal).then(
      value => { if (!controller.signal.aborted) setTools(value); },
      () => { if (!controller.signal.aborted) setUnavailable(true); },
    );
    return () => controller.abort();
  }, [installation.install_id, installation.digest, slot.slot, effect, canWrite, bindingId, invalid]);
  const selectedTool = tools?.find(tool => tool.slug === selected);
  const needsAccount = selectedTool?.connectedAccounts !== undefined;
  const selectedAccount = selectedTool?.connectedAccounts?.find(account => account.id === accountId);
  return <section className="card mcard wa-stack" aria-label={`${slot.slot} ${t("platform tool")}`}>
    <h3>{t("Platform tool")}: {slot.slot}</h3>
    <p className="wa-kicker">{effect === "read" ? t("This slot requests a read tool.") : t("This slot requests a draft tool.")}</p>
    <p className="wa-muted">{t("This app can provide data to the selected tool. Charges use the workspace balance; paid calls require confirmation.")}</p>
    {invalid ? <p className="wa-alert" role="alert">{t("The saved tool grant could not be verified.")}</p> : binding ? <>
      <p className="wa-kicker">{t("Granted tool")}: <code>{binding.config.tool_slug}</code></p>
      {binding.config.connection_id && <p className="wa-muted">{t("Connected account")}: <code>{binding.config.connection_id}</code></p>}
      <p className="wa-muted">{t("Tool availability and price are checked when used.")}</p>
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
            <p className="wa-muted">{t("Listed prices are indicative; check the price before confirming a call.")}</p>
            <Select value={selected} onChange={value => { setSelected(value); setAccountId(""); }} disabled={busy} full
              aria-label={t("Choose platform tool")}
              placeholder={t("Choose platform tool")}
              options={tools.map(tool => ({ value: tool.slug, label: tool.displayName,
                hint: tool.pricing === "request_quote" ? t("Price reviewed per request") :
                  `${tool.price.currency} ${tool.price.amount} ${t("per call")}${tool.unitPrice ? ` · ${tool.unitPrice.currency} ${tool.unitPrice.amount} ${t("per unit")}` : ""}` }))} />
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
