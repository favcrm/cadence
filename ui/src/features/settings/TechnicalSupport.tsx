import { useEffect, useRef, useState, type FormEvent } from "react";
import { useLocale } from "../../lib/locale";
import { onPrivateSessionReset, privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import Link from "../../ui/Link";

type Impact = "blocked" | "degraded" | "question";
type Status = "open" | "in_progress" | "resolved";
type Receipt = { id: string; subject: string; impact: Impact; status: Status; source: "board" | "cli" | "mcp"; createdAt: number; updatedAt: number };
type Page = { cases: Receipt[]; hasMore: boolean };
const CASE_ID = /^SUP-[0-9A-F]{16}$/;

type Draft = { subject: string; impact: Impact; description: string; includeDiagnostics: boolean; requestId: string | null };
const emptyDraft: Draft = { subject: "", impact: "blocked", description: "", includeDiagnostics: false, requestId: null };

// Keep an uncertain submission in memory across navigation and temporary
// verification pauses, but never carry it across a confirmed identity change.
let pendingDraft = emptyDraft;
onPrivateSessionReset(() => { pendingDraft = emptyDraft; });

function receipt(value: unknown): value is Receipt {
  if (!value || typeof value !== "object") return false;
  const r = value as Record<string, unknown>;
  return typeof r.id === "string" && CASE_ID.test(r.id) && typeof r.subject === "string"
    && (r.impact === "blocked" || r.impact === "degraded" || r.impact === "question")
    && (r.status === "open" || r.status === "in_progress" || r.status === "resolved")
    && (r.source === "board" || r.source === "cli" || r.source === "mcp")
    && typeof r.createdAt === "number" && Number.isFinite(r.createdAt)
    && typeof r.updatedAt === "number" && Number.isFinite(r.updatedAt);
}

class SupportError extends Error {
  constructor(readonly status: number) { super(`Support request failed (${status})`); }
}

async function request(path: string, init?: RequestInit, signal?: AbortSignal): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(path, { ...init, credentials: "same-origin", headers: { ...sessionHeaders(), ...init?.headers } }, operation);
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw new SupportError(response.status);
    return body;
  }, signal);
}

function page(value: unknown): Page {
  if (!value || typeof value !== "object") throw new Error("Invalid support list");
  const result = value as Record<string, unknown>;
  if (!Array.isArray(result.cases) || !result.cases.every(receipt) || typeof result.hasMore !== "boolean") throw new Error("Invalid support list");
  return { cases: result.cases, hasMore: result.hasMore };
}

export default function TechnicalSupport({ search }: { search: string }) {
  const { t, locale } = useLocale();
  const id = new URLSearchParams(search).get("case");
  const [{ subject, impact, description, includeDiagnostics }, setDraft] = useState(() => pendingDraft);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [saved, setSaved] = useState<Receipt | null>(null);
  const [cases, setCases] = useState<Receipt[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState(false);
  const [detail, setDetail] = useState<Receipt | null>(null);
  const [detailError, setDetailError] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const savingRef = useRef(false);
  const lifetime = useRef<AbortController | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    lifetime.current = controller;
    return () => { controller.abort(); lifetime.current = null; };
  }, []);

  useEffect(() => {
    if (id) return;
    const controller = new AbortController();
    setLoading(true);
    setListError(false);
    request("/api/support/cases?offset=0", undefined, controller.signal).then((body) => {
      const result = page(body);
      setCases(result.cases);
      setHasMore(result.hasMore);
    }).catch(() => { if (!controller.signal.aborted) setListError(true); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [id, refresh]);

  useEffect(() => {
    setDetail(null);
    setDetailError(false);
    if (!id) return;
    if (!CASE_ID.test(id)) { setDetailError(true); return; }
    const controller = new AbortController();
    request(`/api/support/cases/${encodeURIComponent(id)}`, undefined, controller.signal).then((body) => {
      if (!receipt(body) || body.id !== id) throw new Error("Invalid support receipt");
      setDetail(body);
    }).catch(() => { if (!controller.signal.aborted) setDetailError(true); });
    return () => controller.abort();
  }, [id, refresh]);

  const change = (fields: Partial<Omit<Draft, "requestId">>) => {
    pendingDraft = { ...pendingDraft, ...fields, requestId: null };
    setDraft(pendingDraft);
    setSaveError("");
    setSaved(null);
  };
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (savingRef.current) return;
    if (!subject.trim() || !description.trim()) { setSaveError(t("Enter a subject and description.")); return; }
    savingRef.current = true;
    setSaving(true);
    setSaveError("");
    const controller = lifetime.current;
    const key = pendingDraft.requestId ?? crypto.randomUUID();
    pendingDraft = { ...pendingDraft, requestId: key };
    try {
      const body = await request("/api/support/cases", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Cadence-Board": "1" },
        body: JSON.stringify({ requestId: key, subject, impact, description, includeDiagnostics }),
      }, controller?.signal);
      if (!receipt(body)) throw new Error("Invalid support receipt");
      if (controller?.signal.aborted || pendingDraft.requestId !== key) return;
      setSaved(body);
      pendingDraft = emptyDraft;
      setDraft(emptyDraft);
      setRefresh((n) => n + 1);
    } catch (error) {
      if (!controller?.signal.aborted) setSaveError(error instanceof SupportError && error.status === 501
        ? t("Technical support is not available on this board yet. Your draft was not saved.")
        : t("Could not confirm your request was saved. Your draft is still here; retry without changing it to avoid duplicates."));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };

  const loadMore = async () => {
    if (loading) return;
    setLoading(true); setListError(false);
    const controller = lifetime.current;
    try {
      const result = page(await request(`/api/support/cases?offset=${cases.length}`, undefined, controller?.signal));
      setCases((current) => [...current, ...result.cases]);
      setHasMore(result.hasMore);
    } catch { if (!controller?.signal.aborted) setListError(true); }
    finally { if (!controller?.signal.aborted) setLoading(false); }
  };
  const status = (value: Status) => t(value === "open" ? "Open" : value === "in_progress" ? "In progress" : "Resolved");
  const date = (value: number) => new Date(value).toLocaleString(locale);

  return (
    <main className="px-4 lg:px-8 pt-8 pb-9 max-w-5xl">
      <h1 className="text-section font-semibold text-ink-100">{t("Technical support")}</h1>
      <p className="text-body text-ink-400 mt-2 mb-6">{t("Report a problem with Cadence to Platform Support. It won’t appear on your workspace issue board.")}</p>
      {id ? (
        <section className="card p-5 space-y-3" aria-label={t("Request details")}>
          <Link className="text-accent underline" href="/settings/support">{t("Back to requests")}</Link>
          {detailError ? <p role="alert">{t("Could not load this request. Try again.")} <button type="button" className="underline" onClick={() => setRefresh((n) => n + 1)}>{t("Retry")}</button></p>
            : detail ? <><h2 className="text-body font-semibold">{detail.subject}</h2><p className="num">{detail.id}</p><p>{status(detail.status)}</p><p className="text-secondary text-ink-400">{t("Updated")}: {date(detail.updatedAt)}</p></>
              : <p role="status">{t("Loading requests…")}</p>}
        </section>
      ) : (
        <div className="grid gap-4 lg:grid-cols-[minmax(0,1.6fr)_minmax(16rem,1fr)] items-start">
          <form className="card p-5 grid gap-4" onSubmit={submit} method="post" action="/api/support/cases">
            <h2 className="text-body font-semibold">{t("New request")}</h2>
            {saved && <div role="status" className="bg-ink-800 rounded-md p-3"><strong>{t("Request received")}</strong><p><Link href={`/settings/support?case=${encodeURIComponent(saved.id)}`} className="underline num">{saved.id}</Link> · {status(saved.status)}</p></div>}
            <label className="grid gap-1 text-secondary font-medium">{t("Subject")} <input name="subject" className="field w-full" required maxLength={100} value={subject} disabled={saving} onChange={(e) => change({ subject: e.target.value })} /></label>
            <fieldset><legend className="text-secondary font-medium mb-2">{t("Impact")}</legend><div className="flex flex-wrap gap-3">{(["blocked", "degraded", "question"] as const).map((value) => <label className="flex items-center gap-2 text-secondary" key={value}><input type="radio" name="impact" value={value} checked={impact === value} disabled={saving} onChange={() => change({ impact: value })} />{t(value === "blocked" ? "Blocked" : value === "degraded" ? "Degraded" : "Question")}</label>)}</div></fieldset>
            <div>
              <label className="grid gap-1 text-secondary font-medium">{t("What happened?")} <textarea name="description" className="field w-full" rows={5} required maxLength={4000} value={description} disabled={saving} aria-describedby="support-description-hint" onChange={(e) => change({ description: e.target.value })} /></label>
              <p id="support-description-hint" className="text-secondary text-ink-400 mt-2">{t("Don’t include credentials, customer records or transcripts.")}</p>
            </div>
            <div><label className="flex items-center gap-2 text-secondary"><input type="checkbox" name="includeDiagnostics" checked={includeDiagnostics} disabled={saving} aria-describedby="support-diagnostics-hint" onChange={(e) => change({ includeDiagnostics: e.target.checked })} />{t("Include screen diagnostics")}</label><p id="support-diagnostics-hint" className="text-secondary text-ink-400 mt-2">{t("Opt in to include the screen and Board version. Logs and workspace data are not attached.")}</p></div>
            {saveError && <p role="alert" className="text-secondary text-fail">{saveError}</p>}
            <div><button className="btn btn-primary" type="submit" disabled={saving}>{t(saving ? "Sending…" : "Send request")}</button></div>
          </form>
          <div className="grid min-w-0 gap-4">
            <section className="card p-5 grid gap-2" aria-labelledby="support-next">
              <h2 id="support-next" className="text-body font-semibold">{t("What happens next")}</h2>
              <ol className="list-decimal space-y-1 ps-5 text-secondary text-ink-400">
                <li>{t("You get a case ID.")}</li>
                <li>{t("Platform Support gets an in-app alert and can review the case in their queue.")}</li>
                <li>{t("Check My requests to see when support starts work or resolves your case.")}</li>
              </ol>
            </section>
            <section className="card p-5 grid gap-3" aria-label={t("My requests")}><h2 className="text-body font-semibold">{t("My requests")}</h2>
              {cases.map((item) => <Link key={item.id} href={`/settings/support?case=${encodeURIComponent(item.id)}`} className="block border-t border-ink-700 pt-3"><span className="block font-medium break-words">{item.subject}</span><span className="text-secondary text-ink-400 num">{item.id}</span><span className="block text-secondary">{status(item.status)}</span></Link>)}
              {loading && <p role="status" className="text-secondary text-ink-400">{t("Loading requests…")}</p>}
              {listError && <p role="alert" className="text-secondary">{t("Could not load support requests. Try again.")} <button type="button" className="underline" onClick={() => setRefresh((n) => n + 1)}>{t("Retry")}</button></p>}
              {!loading && !listError && cases.length === 0 && <p className="text-secondary text-ink-400">{t("No support requests yet.")}</p>}
              {hasMore && !loading && <button type="button" className="btn btn-sm" onClick={loadMore}>{t("Load more")}</button>}
            </section>
          </div>
        </div>
      )}
    </main>
  );
}
