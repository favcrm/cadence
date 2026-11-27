import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import Button from "../../ui/Button";
import { decideAccess, FileAccessError, listAccessRequests, requestAccess, shareFile, type AccessRequest, type SharePreview } from "./fileAccess";

// Responses from an earlier private session must never repopulate these views.
function useFence(clear: () => void) {
  const current = useRef(clear);
  current.current = clear;
  const epoch = useRef(0);
  useEffect(() => {
    const stop = onPrivateReadsPaused(() => { epoch.current++; current.current(); });
    return () => { epoch.current++; stop(); };
  }, []);
  return () => {
    const value = epoch.current;
    const generation = privateSessionGeneration();
    return () => value === epoch.current && generation === privateSessionGeneration() && privateReadsAllowed();
  };
}

export function useFileAccessSupport() {
  const [supported, setSupported] = useState(false);
  const fence = useFence(() => setSupported(false));
  useEffect(() => {
    const controller = new AbortController();
    const valid = fence();
    void listAccessRequests(controller.signal).then(({ paused }) => { if (valid()) setSupported(!paused); }).catch(() => { if (valid()) setSupported(false); });
    return () => controller.abort();
  }, []);
  return supported;
}

function statusLabel(status: string, t: (s: string) => string) {
  switch (status) {
    case "pending": return t("Pending");
    case "granted": return t("Granted");
    case "declined": return t("Declined");
    case "revoked": return t("Revoked");
    default: return t("Available");
  }
}

export function AccessRequestButton({ id, version }: { id: string; version: number }) {
  const t = useTranslate();
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const [busy, setBusy] = useState(false);
  const fence = useFence(() => { setStatus(null); setError(false); setBusy(false); });
  const request = async () => {
    const valid = fence(); setBusy(true); setError(false);
    try { const next = await requestAccess(id, version); if (valid()) setStatus(next); }
    catch { if (valid()) setError(true); }
    finally { if (valid()) setBusy(false); }
  };
  return <div className="grid gap-2">
    <Button disabled={busy} onClick={() => void request()}>{t("Request access")}</Button>
    {status && <p role="status">{t("Access request status")}: {statusLabel(status, t)}</p>}
    {error && <p role="alert" className="text-fail">{t("Could not request access. Try again.")}</p>}
  </div>;
}

export function FileSharing({ id, version, href, readOnly, onConflict }: { id: string; version: number; href: string; readOnly: boolean; onConflict: () => void }) {
  const t = useTranslate();
  const [ticket, setTicket] = useState("");
  const [preview, setPreview] = useState<SharePreview | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const fence = useFence(() => { setTicket(""); setPreview(null); setMessage(""); setError(""); setBusy(false); });
  const copy = async () => {
    const valid = fence();
    try { await navigator.clipboard.writeText(new URL(href, window.location.href).href); if (valid()) setMessage(t("Private link copied. It does not grant access.")); }
    catch { if (valid()) setError(t("Could not copy the private link.")); }
  };
  const submit = async (confirm: boolean) => {
    if (busy || !ticket.trim() || (confirm && !preview)) return;
    const valid = fence(); setBusy(true); setError(""); setMessage("");
    if (!confirm) setPreview(null);
    try {
      const result = await shareFile(id, version, confirm ? preview!.ticket : ticket.trim(), confirm ? preview!.audience : undefined);
      if (!valid()) return;
      if (result.status === "preview") setPreview(result);
      else { setPreview(null); setMessage(t(result.status === "linked" ? "This version was linked without changing access." : "This version was shared with ticket readers.")); }
    } catch (e) {
      if (!valid()) return;
      if (e instanceof FileAccessError && e.code === "revision_conflict") { setPreview(null); onConflict(); setError(t("The audience changed. Reload and confirm again.")); }
      else if (e instanceof FileAccessError && e.code === "document_not_approved") setError(t("Get approval for this version before sharing."));
      else if (e instanceof FileAccessError && e.code === "document_archived") setError(t("Restore this document before sharing."));
      else setError(t("Could not share this version. Try again."));
    } finally { if (valid()) setBusy(false); }
  };
  return <section className="card grid gap-2 p-3">
    <div><Button onClick={() => void copy()}>{t("Copy private link")}</Button></div>
    {!readOnly && <>
      <form className="grid gap-2" onSubmit={(event) => { event.preventDefault(); void submit(false); }}>
        <label htmlFor="share-ticket">{t("Ticket ID")}</label>
        <input id="share-ticket" className="field" value={ticket} required disabled={busy} onChange={(event) => { setTicket(event.target.value); setPreview(null); }} />
        <div><Button type="submit" disabled={busy}>{t("Link to ticket")}</Button></div>
      </form>
      {preview && <div className="grid gap-2">
        <p>{t("Share this version with ticket readers?")}</p>
        <p>{t("Ticket")}: {preview.ticket} · {t("Project")}: {preview.audience.project} · {t("Visibility")}: {t(preview.audience.visibility === "private" ? "Private" : "Workspace")}</p>
        <p>{t("Audience revision")}: {preview.audience.revision ?? "—"}</p>
        <div><Button variant="primary" disabled={busy} onClick={() => void submit(true)}>{t("Share this version with ticket readers")}</Button></div>
      </div>}
    </>}
    {message && <p role="status">{message}</p>}
    {error && <p role="alert" className="text-fail">{error}</p>}
  </section>;
}

export function AccessInbox({ readOnly }: { readOnly: boolean }) {
  const t = useTranslate();
  const [rows, setRows] = useState<AccessRequest[] | null>(null);
  const [error, setError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);
  const fence = useFence(() => { setRows(null); setError(false); setBusy(false); });
  useEffect(() => {
    const controller = new AbortController(); const valid = fence();
    void listAccessRequests(controller.signal).then(({ requests }) => { if (valid()) { setRows(requests); setError(false); } }).catch((e) => {
      if (!valid()) return;
      setRows(null);
      if (!(e instanceof FileAccessError && [400, 404, 501].includes(e.status))) setError(true);
    });
    return () => controller.abort();
  }, [tick]);
  const decide = async (row: AccessRequest, decision: "grant" | "decline" | "revoke") => {
    const valid = fence(); setBusy(true); setError(false);
    try { await decideAccess(row, decision); if (valid()) setTick((n) => n + 1); }
    catch { if (valid()) { setError(true); setTick((n) => n + 1); } }
    finally { if (valid()) setBusy(false); }
  };
  if (!rows) return error ? <p role="alert" className="text-fail">{t("Could not load access requests.")}</p> : null;
  return <section className="card grid gap-2 p-3">
    <h2 className="text-cardtitle font-semibold text-ink-100">{t("File access requests")}</h2>
    {!rows.length && <p>{t("No file access requests.")}</p>}
    {rows.map((row) => <div key={`${row.id}:${row.version}:${row.requester}`} className="border-b border-ink-800 py-2">
      <p>{row.title || row.id} · v{row.version} · {row.requester} · {statusLabel(row.status, t)}</p>
      {!readOnly && <div className="flex flex-wrap gap-2">
        {row.status === "pending" && <><Button disabled={busy} onClick={() => void decide(row, "grant")}>{t("Grant access")}</Button><Button disabled={busy} onClick={() => void decide(row, "decline")}>{t("Decline")}</Button></>}
        {row.status === "granted" && <Button disabled={busy} onClick={() => void decide(row, "revoke")}>{t("Revoke access")}</Button>}
      </div>}
    </div>)}
    {error && <p role="alert" className="text-fail">{t("Could not update access. Requests have been reloaded.")}</p>}
  </section>;
}
