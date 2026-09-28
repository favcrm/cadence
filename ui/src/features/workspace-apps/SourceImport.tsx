import { useEffect, useState, type FormEvent } from "react";
import Button from "../../ui/Button";
import Select, { type SelectOption } from "../../ui/Select";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { ApiError } from "../../lib/api";
import { workspaceApps, type CapabilityQuote } from "./workspaceApps";

export interface SourceImportValues {
  profileHandle: string;
  ownerPm: string;
  writer: string;
}

export function SourceImport({ installId, contextId, bindingDigest, workers, managers, busy, error, onDenied, onClose, onCreate }: {
  installId: string;
  contextId?: string;
  bindingDigest: string;
  workers: SelectOption[];
  managers: SelectOption[];
  busy: boolean;
  error: string | null;
  onDenied: () => void;
  onClose: () => void;
  onCreate: (values: SourceImportValues) => void;
}) {
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
        setQuoteError("The source connection or provider price changed. Refresh Settings before creating a plan."); return;
      }
      setQuote(value);
    }).catch(reason => {
      if (controller.signal.aborted) return;
      if (reason instanceof ApiError && [401, 403].includes(reason.status)) { onDenied(); return; }
      setQuoteError(reason instanceof Error ? reason.message : "Could not obtain the provider’s current price.");
    });
    return () => controller.abort();
  }, [installId, contextId, bindingDigest, onDenied]);
  const eligibleWorkers = ownerPm ? workers.filter(worker => worker.group === ownerPm) : [];
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const handle = profileHandle.trim().replace(/^@/, "");
    const invalid = !/^[A-Za-z0-9][A-Za-z0-9_.]{0,29}$/.test(handle)
      ? "Enter an Instagram handle with letters, numbers, dots or underscores."
      : !managers.some(manager => manager.value === ownerPm)
        ? "Choose the responsible PM."
        : !eligibleWorkers.some(worker => worker.value === writer)
          ? "Choose a registered reader from that PM’s team."
          : null;
    setValidation(invalid);
    if (!invalid && !busy && quote) onCreate({ profileHandle: handle, ownerPm, writer });
  };
  return <WorkspaceDialog title="Find Instagram source" onClose={onClose}>
    <form className="wa-stack" onSubmit={submit}>
      <p className="wa-muted">Read recent public posts through this app’s Instagram source connection. Cadence shows the current provider charge below; you approve the exact frozen price and plan before a worker starts.</p>
      {quote ? <p className="wa-alert">Current provider charge for one read: <strong>USD {(quote.quote.total_price_micros / 1_000_000).toFixed(6)}</strong>. The plan will freeze this price; a change before execution stops the call.</p> : !quoteError && <p className="wa-muted" role="status">Checking provider price…</p>}
      {quoteError && <p className="wa-alert" data-tone="fail" role="alert">{quoteError}</p>}
      {(error || validation) && <p className="wa-alert" data-tone="fail" role="alert">{error || validation}</p>}
      <div className="wa-field">
        <label htmlFor="wa-profile-handle">Public Instagram handle</label>
        <input id="wa-profile-handle" className="wa-input" value={profileHandle} onChange={event => setProfileHandle(event.target.value)} placeholder="juicysuite_crm" maxLength={31} required disabled={busy} />
      </div>
      <div className="wa-fields">
        <div className="wa-field">
          <label htmlFor="wa-source-owner">Responsible PM</label>
          <Select id="wa-source-owner" value={ownerPm} onChange={value => { setOwnerPm(value); setWriter(""); }} options={managers} placeholder="Choose PM" disabled={busy} full />
        </div>
        <div className="wa-field">
          <label htmlFor="wa-source-reader">Reader</label>
          <Select id="wa-source-reader" value={writer} onChange={setWriter} options={eligibleWorkers} placeholder={ownerPm ? "Choose reader" : "Choose a PM first"} disabled={busy || !ownerPm} full />
        </div>
      </div>
      <Button type="submit" variant="primary" disabled={!quote} loading={busy}>Create source plan</Button>
    </form>
  </WorkspaceDialog>;
}
