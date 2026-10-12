import { useEffect, useState, type FormEvent } from "react";
import Button from "../../ui/Button";
import Select, { type SelectOption } from "../../ui/Select";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { ApiError } from "../../lib/api";
import { workspaceApps, type CapabilityQuote } from "./workspaceApps";
import { useTranslate } from "../../lib/locale";

const CONNECTION_CHANGED = "The source connection changed or is unavailable. Refresh Settings before creating a plan.";

export interface SourceImportValues {
  profileHandle: string;
  ownerPm: string;
  writer: string;
}

export function SourceImport({ installId, contextId, bindingDigest, workers, managers, busy, error, host, onDenied, onClose, onCreate }: {
  installId: string;
  contextId?: string;
  bindingDigest: string;
  workers: SelectOption[];
  managers: SelectOption[];
  busy: boolean;
  error: string | null;
  /** CAD-1171: a host-executed workflow runs for the operator's own click — no PM/reader team. */
  host?: boolean;
  onDenied: () => void;
  onClose: () => void;
  onCreate: (values: SourceImportValues) => void;
}) {
  const t = useTranslate();
  const [profileHandle, setProfileHandle] = useState("");
  const [ownerPm, setOwnerPm] = useState("");
  const [writer, setWriter] = useState("");
  const [validation, setValidation] = useState<string | null>(null);
  const [quote, setQuote] = useState<CapabilityQuote | null>(null);
  const [quoteError, setQuoteError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    setQuote(null); setQuoteError(null);
    void workspaceApps.bindingQuote(installId, "source", contextId, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (value.slot !== "source" || value.binding_digest !== bindingDigest || value.quote.schema !== 1 || value.quote.currency !== "USD" || !Number.isSafeInteger(value.quote.total_price_micros) || value.quote.total_price_micros <= 0) {
        setQuoteError(CONNECTION_CHANGED); return;
      }
      setQuote(value);
    }).catch(reason => {
      if (controller.signal.aborted) return;
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setQuoteError(reason instanceof Error ? reason.message : "Could not reach the source provider.");
    });
    return () => controller.abort();
  }, [installId, contextId, bindingDigest, onDenied]);
  const eligibleWorkers = ownerPm ? workers.filter(worker => worker.group === ownerPm) : [];
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const handle = profileHandle.trim().replace(/^@/, "");
    const invalid = !/^[A-Za-z0-9][A-Za-z0-9_.]{0,29}$/.test(handle)
      ? "Enter an Instagram handle with letters, numbers, dots or underscores."
      : host
        ? null
        : !managers.some(manager => manager.value === ownerPm)
          ? "Choose the responsible PM."
          : !eligibleWorkers.some(worker => worker.value === writer)
            ? "Choose a registered reader from that PM’s team."
            : null;
    setValidation(invalid);
    if (!invalid && !busy && quote) onCreate({ profileHandle: handle, ownerPm, writer });
  };
  return <WorkspaceDialog title={t("Find Instagram source")} onClose={onClose}>
    <form className="wa-stack" onSubmit={submit}>
      <p className="wa-muted">{host
        ? t("Read recent public posts through this app’s Instagram source connection. This click starts the read — no team and no separate plan approval.")
        : t("Read recent public posts through this app’s Instagram source connection. You approve the frozen plan before a worker starts.")}</p>
      {quote ? <p className="wa-alert">{host ? t("The source connection is ready.") : t("The source connection is ready — the plan needs your approval before the read starts.")}</p> : !quoteError && <p className="wa-muted" role="status">{t("Checking the source connection…")}</p>}
      {quoteError && <p className="wa-alert" data-tone="fail" role="alert">{quoteError === CONNECTION_CHANGED ? t(CONNECTION_CHANGED) : quoteError}</p>}
      {(error || validation) && <p className="wa-alert" data-tone="fail" role="alert">{error || t(validation ?? "")}</p>}
      <div className="wa-field">
        <label htmlFor="wa-profile-handle">{t("Public Instagram handle")}</label>
        <input id="wa-profile-handle" className="wa-input" value={profileHandle} onChange={event => setProfileHandle(event.target.value)} placeholder="juicysuite_crm" maxLength={31} required disabled={busy} />
      </div>
      {!host && <div className="wa-fields">
        <div className="wa-field">
          <label htmlFor="wa-source-owner">{t("Responsible PM")}</label>
          <Select id="wa-source-owner" value={ownerPm} onChange={value => { setOwnerPm(value); setWriter(""); }} options={managers} placeholder={t("Choose PM")} disabled={busy} full />
        </div>
        <div className="wa-field">
          <label htmlFor="wa-source-reader">{t("Reader")}</label>
          <Select id="wa-source-reader" value={writer} onChange={setWriter} options={eligibleWorkers} placeholder={ownerPm ? t("Choose reader") : t("Choose a PM first")} disabled={busy || !ownerPm} full />
        </div>
      </div>}
      <Button type="submit" variant="primary" disabled={!quote} loading={busy}>{t("Create source plan")}</Button>
    </form>
  </WorkspaceDialog>;
}
