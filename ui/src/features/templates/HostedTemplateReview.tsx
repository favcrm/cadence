import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { useLocale, useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import Button from "../../ui/Button";
import DrawerShell from "../app-shell/shared/DrawerShell";
import Field from "../app-shell/shared/Field";
import { htmlFrameDocument } from "../canvas/ArtifactView";
import { base64Blob, templatesApi, type TemplateDirectoryEntry, type TemplateSpec, type TemplateSummary, type TemplateView } from "./templatesApi";

export type HostedTemplateReviewTarget = { name: string; version: number; digest: string };
export type HostedTemplateRail = (onReview: () => void) => ReactNode;

export interface HostedTemplateList {
  pending: TemplateSummary[];
  approved: TemplateSummary[];
  loading: boolean;
  error: boolean;
  refresh: () => void;
  open: (version: TemplateSummary, opener?: HTMLElement) => void;
}

type Preview = { kind: "pdf"; url: string; rows: number } | { kind: "html"; html: string; rows: number };
type SampleRows = 1 | 36;

function hasFinancialSpec(view: TemplateView, target: HostedTemplateReviewTarget): boolean {
  const spec = view.spec;
  return matches(view.template, target) && (spec?.kind === "invoice" || spec?.kind === "quotation") && spec.kind === view.template.kind &&
    Array.isArray(spec.fields) && typeof spec.calculation?.confirmed === "boolean" &&
    typeof spec.calculation.currency === "string" && Number.isInteger(spec.calculation.tax_rate_bps) &&
    Number.isInteger(spec.calculation.discount_bps);
}

function matches(template: TemplateSummary | undefined, target: HostedTemplateReviewTarget): boolean {
  return !!template && template.name === target.name && template.version === target.version && template.content_sha256 === target.digest && (template.status === "draft" || template.status === "approved");
}

function targetOf(version: TemplateSummary): HostedTemplateReviewTarget {
  return { name: version.name, version: version.version, digest: version.content_sha256 };
}

let publishedPendingTemplates = 0;
const templatePendingListeners = new Set<() => void>();

function setPublishedPendingTemplates(next: number) {
  if (publishedPendingTemplates === next) return;
  publishedPendingTemplates = next;
  for (const listener of templatePendingListeners) listener();
}

function resetPublishedPendingTemplates() {
  setPublishedPendingTemplates(0);
}

onPrivateReadsPaused(resetPublishedPendingTemplates);
onPrivateSessionReset(resetPublishedPendingTemplates);

/** Latest hosted draft template count published while Home's template review is active. */
export function useHostedTemplatePendingCount(): number {
  return useSyncExternalStore(
    (listener) => {
      templatePendingListeners.add(listener);
      return () => templatePendingListeners.delete(listener);
    },
    () => publishedPendingTemplates,
  );
}

/** One Home-owned review surface. Every continuation is bound to its selection and private session. */
export function useHostedTemplateReview({ enabled, canApprove, onCompose }: {
  enabled: boolean;
  canApprove: boolean;
  onCompose?: (text: string) => void;
}): { rail: HostedTemplateRail | undefined; templateList: HostedTemplateList | undefined; drawer: ReactNode; pendingCount: number | undefined; open: (target: HostedTemplateReviewTarget, opener?: HTMLElement) => void; refresh: () => void } {
  const t = useTranslate();
  const [directory, setDirectory] = useState<TemplateDirectoryEntry[] | null>(null);
  const [listError, setListError] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [target, setTarget] = useState<HostedTemplateReviewTarget | null>(null);
  const [view, setView] = useState<TemplateView | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [detail, setDetail] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [source, setSource] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewDetail, setPreviewDetail] = useState<string | null>(null);
  const [rows, setRows] = useState<SampleRows | null>(null);
  const selection = useRef(0);
  const listRequest = useRef(0);
  const sampleRequest = useRef(0);
  const url = useRef<string | null>(null);
  const active = useRef(enabled);
  const approvalAllowed = useRef(canApprove);
  const openerRef = useRef<HTMLElement | undefined>(undefined);
  active.current = enabled;
  approvalAllowed.current = canApprove;

  const clearPreview = useCallback(() => {
    sampleRequest.current += 1;
    if (url.current) URL.revokeObjectURL(url.current);
    url.current = null;
    setPreview(null);
    setPreviewBusy(false);
    setPreviewError(null);
    setPreviewDetail(null);
  }, []);

  const current = useCallback((serial: number, generation: number) =>
    active.current && privateReadsAllowed() && selection.current === serial && privateSessionGeneration() === generation, []);

  const refresh = useCallback(() => {
    if (!active.current || !privateReadsAllowed()) return;
    const request = ++listRequest.current;
    const generation = privateSessionGeneration();
    setListError(false);
    void templatesApi.listAll().then((result) => {
      if (active.current && privateReadsAllowed() && request === listRequest.current && generation === privateSessionGeneration()) {
        if (!Array.isArray(result?.templates) || result.templates.some((entry) => !Array.isArray(entry.versions))) {
          setDirectory(null);
          setListError(true);
          return;
        }
        setDirectory(result.templates);
      }
    }).catch(() => {
      if (active.current && privateReadsAllowed() && request === listRequest.current && generation === privateSessionGeneration()) {
        setDirectory(null);
        setListError(true);
      }
    });
  }, []);

  const reset = useCallback(() => {
    selection.current += 1;
    listRequest.current += 1;
    clearPreview();
    setDirectory(null);
    setListError(false);
    setTarget(null);
    setView(null);
    setLoading(false);
    setBusy(false);
    openerRef.current = undefined;
    setError(null);
    setDetail(null);
    setNotice(null);
    setSource("");
  }, [clearPreview]);

  useEffect(() => {
    const stop = onPrivateReadsPaused(reset);
    return () => { stop(); reset(); };
  }, [reset]);
  useEffect(() => {
    if (enabled) refresh();
    else reset();
  }, [enabled, refresh, reset]);

  const open = useCallback((next: HostedTemplateReviewTarget, opener?: HTMLElement) => {
    if (!active.current || !privateReadsAllowed()) return;
    selection.current += 1;
    clearPreview();
    if (!opener?.closest('[data-drawer="template-review"]')) openerRef.current = opener;
    setTarget({ ...next });
    setView(null);
    setLoading(true);
    setBusy(false);
    setError(null);
    setDetail(null);
    setNotice(null);
    setSource("");
    setRows(null);
  }, [clearPreview]);

  const loadSample = useCallback(async (selected: HostedTemplateReviewTarget, count: SampleRows, format: "pdf" | "html", serial: number, generation: number) => {
    clearPreview();
    setRows(count);
    const request = sampleRequest.current;
    setPreviewBusy(true);
    try {
      const sample = await templatesApi.sample(selected.name, selected.version, format, count);
      if (!current(serial, generation) || request !== sampleRequest.current) return;
      if (sample?.sample !== true || sample.template !== selected.name || sample.version !== selected.version || sample.content_digest !== selected.digest || !Array.isArray(sample.context?.items) || sample.context.items.length !== count) {
        throw new Error("receipt");
      }
      if (format === "pdf") {
        if (!sample.pdf_base64 || !atob(sample.pdf_base64.slice(0, 8)).startsWith("%PDF-")) throw new Error("format");
        const objectUrl = URL.createObjectURL(base64Blob(sample.pdf_base64, "application/pdf"));
        url.current = objectUrl;
        setPreview({ kind: "pdf", url: objectUrl, rows: count });
      } else {
        const html = sample.html ?? (sample.html_base64 ? new TextDecoder().decode(Uint8Array.from(atob(sample.html_base64), (c) => c.charCodeAt(0))) : undefined);
        if (html === undefined) throw new Error("format");
        setPreview({ kind: "html", html, rows: count });
      }
    } catch (cause) {
      if (!current(serial, generation) || request !== sampleRequest.current) return;
      setPreviewError(cause instanceof Error && cause.message === "receipt" ? "The sample receipt did not match the selected version, digest and row count. No preview is shown." : format === "pdf" ? "Sample PDF is unavailable." : "SAMPLE HTML is unavailable for this version.");
      if (cause instanceof Error && cause.message !== "receipt" && cause.message !== "format") setPreviewDetail(cause.message);
    } finally {
      if (current(serial, generation) && request === sampleRequest.current) setPreviewBusy(false);
    }
  }, [clearPreview, current]);
  useEffect(() => {
    if (!target || !enabled || !privateReadsAllowed()) return;
    const serial = selection.current;
    const generation = privateSessionGeneration();
    void templatesApi.version(target.name, target.version).then((result) => {
      if (!current(serial, generation)) return;
      if (!result || !hasFinancialSpec(result, target)) throw new Error("mismatch");
      setView(result);
      setLoading(false);
      if (result.template.status === "draft") void loadSample(target, 1, "pdf", serial, generation);
    }).catch((cause: unknown) => {
      if (!current(serial, generation)) return;
      setLoading(false);
      setError(cause instanceof Error && cause.message === "mismatch" ? "The selected version or digest did not match the server response. Nothing can be approved or used." : "The selected template could not be loaded.");
      if (cause instanceof Error && cause.message !== "mismatch") setDetail(cause.message);
    });
  }, [target, enabled, current, loadSample]);

  useLayoutEffect(() => {
    if (!enabled || !target) return;
    // Approve and retry can remove the focused control. Keep Escape and Tab
    // in this drawer even when a receipt replaces the action footer.
    const root = document.querySelector('[data-drawer="template-review"]');
    if (root && !root.contains(document.activeElement)) {
      root.querySelector<HTMLElement>(".crm-drawer-title")?.focus({ preventScroll: true });
    }
  }, [enabled, target, view, loading, previewBusy]);

  const close = () => {
    selection.current += 1;
    const closedSelection = selection.current;
    clearPreview();
    setTarget(null);
    setView(null);
    const opener = openerRef.current;
    window.setTimeout(() => {
      if (selection.current !== closedSelection || !active.current || !privateReadsAllowed()) return;
      if (opener?.isConnected && opener.getClientRects().length > 0) {
        opener.focus({ preventScroll: true });
        return;
      }
      const fallback = Array.from(document.querySelectorAll<HTMLElement>(".needbtn, .needsrail .todo-tabs > button, .slimrail"))
        .find((element) => element.isConnected && element.getClientRects().length > 0);
      fallback?.focus();
    }, 10);
  };

  const compose = (action: "revise" | "use") => {
    if (!onCompose || !target || !view || !matches(view.template, target) || busy || !privateReadsAllowed() || !active.current) return;
    if (action === "use" && view.template.status !== "approved") return;
    const text = action === "use"
      ? `${t("Please prepare an unsent draft using the approved template")} ${JSON.stringify(target.name)} v${target.version} (SHA-256 ${target.digest}). ${t("Ask me for the document details and do not issue or send anything.")}`
      : `${t("Please revise the template")} ${JSON.stringify(target.name)} v${target.version} (SHA-256 ${target.digest}). ${t("Ask me what to change and create a new draft for review; do not approve or send anything.")}`;
    selection.current += 1;
    clearPreview();
    setTarget(null);
    setView(null);
    onCompose(text);
  };

  const approve = async () => {
    if (!target || !view || !matches(view.template, target) || view.template.status !== "draft" || !view.spec.calculation.confirmed || !source.trim() || busy || !active.current || !approvalAllowed.current || !privateReadsAllowed()) return;
    const selected = target;
    const serial = selection.current;
    const generation = privateSessionGeneration();
    setBusy(true);
    setError(null);
    setDetail(null);
    setNotice(null);
    try {
      const receipt = await templatesApi.approve(selected.name, selected.version, selected.digest, source.trim());
      if (!current(serial, generation)) return;
      if (!matches(receipt?.template, selected) || receipt.template.status !== "approved") throw new Error("receipt");
      setView((old) => old ? { ...old, template: receipt.template } : old);
      setNotice("This exact version is approved for reuse. No document was issued or sent.");
      refresh();
    } catch (cause) {
      if (!current(serial, generation)) return;
      setError("Approval could not be confirmed. Rechecking the exact version…");
      setView(null);
      clearPreview();
      try {
        const reread = await templatesApi.version(selected.name, selected.version);
        if (!current(serial, generation)) return;
        if (reread && hasFinancialSpec(reread, selected)) {
          setView(reread);
          if (reread.template.status === "approved") {
            setError(null);
            setNotice("Rechecked: this exact version is approved for reuse.");
          } else {
            setError("This version is still a draft. Nothing was approved.");
            if (cause instanceof Error && cause.message !== "receipt") setDetail(cause.message);
          }
          setRows(null);
        } else {
          setError("The re-read did not match the selected version and digest. Reload before trying again.");
        }
      } catch {
        if (current(serial, generation)) setError("The approval outcome is unknown. Reload this exact version before trying again.");
      }
      refresh();
    } finally {
      if (current(serial, generation)) setBusy(false);
    }
  };

  const { byName, relevant, pendingTemplates, approvedTemplates, pendingCount } = useMemo(() => {
    const byName = new Map<string, TemplateSummary[]>();
    const pending: TemplateSummary[] = [];
    const approved: TemplateSummary[] = [];
    for (const entry of directory ?? []) {
      if (!entry || (entry.kind !== "quotation" && entry.kind !== "invoice")) continue;
      const versions = (entry.versions ?? []).filter((v) =>
        v && v.name === entry.name && Number.isSafeInteger(v.version) && v.version > 0 &&
        /^[a-f0-9]{64}$/.test(v.content_sha256) && (v.status === "draft" || v.status === "approved"),
      ).sort((a, b) => b.version - a.version);
      byName.set(entry.name, versions);
      const active = versions.find((v) => v.status === "approved");
      const draft = canApprove && versions.find((v) => v.status === "draft" && (!active || v.version > active.version));
      if (draft) pending.push(draft);
      if (active) approved.push(active);
    }
    return { byName, relevant: [...pending, ...approved], pendingTemplates: pending, approvedTemplates: approved, pendingCount: pending.length };
  }, [directory, canApprove]);

  const templateList: HostedTemplateList | undefined = enabled ? {
    pending: pendingTemplates,
    approved: approvedTemplates,
    loading: !directory && !listError,
    error: listError,
    refresh,
    open: (version, opener) => open(targetOf(version), opener),
  } : undefined;
  const shown = showAll ? relevant : relevant.slice(0, 5);
  const history = target ? byName.get(target.name)?.filter((v) => canApprove || v.status === "approved") ?? [] : [];
  const needsApproval = canApprove && view?.template.status === "draft";

  const rail: HostedTemplateRail | undefined = enabled ? (onReview) => (
    <section aria-label={t("Template approvals")} data-template-approvals>
      <h3 className="px-3 pt-3 text-label font-medium text-ink-300">{t("Template approvals")}</h3>
      {listError && <p className="px-3 py-2 text-label text-warn" role="alert">{t("Templates could not be loaded.")} <button type="button" className="lnk" onClick={refresh}>{t("Retry")}</button></p>}
      {!directory && !listError && <p className="px-3 py-2 text-label text-ink-400" role="status">{t("Loading templates…")}</p>}
      {directory && relevant.length === 0 && <p className="px-3 py-2 text-label text-ink-400">{t("No templates available for review or reuse.")}</p>}
      <ul className="todo-list">
        {shown.map((version) => <li key={`${version.name}:${version.version}`} className="px-3 py-2.5 border-b border-ink-800 flex items-center justify-between gap-2 min-w-0" data-record-id={`template:${version.name}:${version.version}`}>
          <div className="min-w-0"><span className="block text-label font-medium text-ink-100 truncate">{version.name} · v{version.version}</span><span className="block text-micro text-ink-400">{version.status === "draft" ? t("Draft awaiting review") : t("Approved for reuse")}</span></div>
          <button type="button" className="btn btn-sm btn-secondary" aria-label={`${t(version.status === "draft" ? "Review" : "Open")} ${version.name} v${version.version}`} onClick={(event) => {
            open(targetOf(version), event.currentTarget);
            onReview();
          }}>{t(version.status === "draft" ? "Review" : "Open")}</button>
        </li>)}
      </ul>
      {relevant.length > 5 && <button type="button" className="lnk px-3 py-2" onClick={() => setShowAll(!showAll)}>{showAll ? t("Show less") : t("Show all templates")}</button>}
    </section>
  ) : undefined;

  const drawer = enabled && target ? <DrawerShell
    kind="template-review" label={t("Template review")} kicker={t("Template review")} scrim="board" modal trapFocus size="lg" preventClose={busy}
    title={`${target.name} · v${target.version}`}
    pills={view && <span className="s-status-pill" data-tone={view.template.status === "approved" ? "ok" : "warn"}>
      {t(view.template.status === "approved" ? "Approved for reuse" : "Draft awaiting review")}
    </span>}
    onClose={close}
    secondary={view && onCompose && <Button variant="ghost" className="max-sm:min-h-11" disabled={busy} onClick={() => compose("revise")}>{t("Ask agent to revise")}</Button>}
    primary={needsApproval && view
      ? <Button variant="primary" className="max-sm:min-h-11" loading={busy} disabled={!source.trim() || !view.spec.calculation.confirmed} onClick={() => void approve()}>{t("Approve")} v{target.version} {t("for reuse")}</Button>
      : view?.template.status === "approved" && onCompose
        ? <Button variant="primary" className="max-sm:min-h-11" disabled={busy} onClick={() => compose("use")}>{t("Use template")}</Button>
        : undefined}
  >
    <div className="space-y-5">
      {loading && <p role="status">{t("Loading the exact template version…")}</p>}
      {error && <p role="alert" className="text-fail break-words">{t(error)} {detail}</p>}
      {error && !view && !busy && !loading && <Button size="sm" variant="secondary" onClick={() => open(target, openerRef.current)}>{t("Reload exact version")}</Button>}
      {notice && <p role="status">{t(notice)}</p>}
      {view && <>
        <section aria-labelledby="template-review-sample" className="space-y-3 rounded-lg border border-ink-700 bg-ink-900 p-4">
          <div>
            <h4 id="template-review-sample" className="font-semibold text-ink-100">{t("Sample preview")}</h4>
            <p className="text-label text-ink-300">{t("Sample only. Do not send.")}</p>
          </div>
          <div className="flex flex-wrap gap-2" role="group" aria-label={t("Sample length")}>
            <Button className="max-sm:min-h-11" aria-pressed={rows === 1} disabled={previewBusy || busy} onClick={() => void loadSample(target, 1, "pdf", selection.current, privateSessionGeneration())}>
              <span className={rows === 1 ? "underline underline-offset-4" : undefined}>{t("Short · 1 row")}</span>
            </Button>
            <Button className="max-sm:min-h-11" aria-pressed={rows === 36} disabled={previewBusy || busy} onClick={() => void loadSample(target, 36, "pdf", selection.current, privateSessionGeneration())}>
              <span className={rows === 36 ? "underline underline-offset-4" : undefined}>{t("Continuation · 36 rows")}</span>
            </Button>
          </div>
          <p role="status" className={previewBusy ? "text-label text-ink-300" : "sr-only"}>
            {previewBusy ? t("Rendering sample…") : preview ? `${t("Verified sample rows")}: ${preview.rows}` : ""}
          </p>
          {previewError && <p role="alert" className="text-warn wrap-anywhere">{t(previewError)} {previewDetail}</p>}
          {previewError && <Button className="max-sm:min-h-11" onClick={() => void loadSample(target, 1, "html", selection.current, privateSessionGeneration())} disabled={previewBusy || busy}>{t("Preview as HTML")}</Button>}
          {preview?.kind === "pdf" && <div className="flex flex-wrap gap-2">
            <a className="btn max-sm:min-h-11" href={preview.url} target="_blank" rel="noopener noreferrer" aria-label={t("Open sample PDF in a new tab")}>
              {t("Open sample PDF")} <span aria-hidden="true">↗</span>
            </a>
            <a className="btn btn-ghost max-sm:min-h-11" href={preview.url} download={`${target.name}-v${target.version}-sample.pdf`} aria-label={t("Download SAMPLE PDF")}>{t("Download")}</a>
          </div>}
          {preview?.kind === "html" && <iframe title={`${t("SAMPLE HTML")} · ${target.name} v${target.version}`} srcDoc={htmlFrameDocument(preview.html)} sandbox="" referrerPolicy="no-referrer" className="w-full min-h-96" />}
        </section>
        <CalculationRules calculation={view.spec.calculation} />
        <details className="border-y border-ink-700">
          <summary tabIndex={0} className="cursor-pointer py-3 font-medium text-ink-100">{t("Fields and version details")}</summary>
          <div className="space-y-3 pb-3">
            <h4 className="font-medium text-ink-100">{t("Fields")}</h4>
            <ul className="grid gap-2 text-label">
              {view.spec.fields.map((field) => <li className="wrap-anywhere" key={field.key}>
                <span className="block font-medium text-ink-100">{field.label}</span>
                <span className="text-ink-300"><code>{field.key}</code> · {t(field.kind)}{field.required ? ` · ${t("Required")}` : ""}</span>
              </li>)}
            </ul>
            {history.length > 1 && <div>
              <h4 className="font-medium text-ink-100">{t("Version history")}</h4>
              <ul className="mt-1 grid gap-1 text-label">{history.map((version) => <li key={version.version}>{version.version === target.version
                ? <span aria-current="true">v{version.version} · {t(version.status === "approved" ? "Approved" : "Draft")}</span>
                : <button type="button" disabled={busy} className="lnk" onClick={(event) => open(targetOf(version), event.currentTarget)}>v{version.version} · {t(version.status === "approved" ? "Approved" : "Draft")}</button>}</li>)}</ul>
            </div>}
            <dl className="sdetail">
              {view.template.based_on !== null && <div><dt>{t("Based on")}</dt><dd>v{view.template.based_on}</dd></div>}
              {view.template.approved_source && <div><dt>{t("Approval source")}</dt><dd>{view.template.approved_source}</dd></div>}
              <div><dt>SHA-256</dt><dd><code>{target.digest}</code></dd></div>
            </dl>
          </div>
        </details>
        {needsApproval && !view.spec.calculation.confirmed && <p className="text-warn">{t("Confirm the calculation rules in a new draft before approving.")}</p>}
        {needsApproval && <Field
          label={t("Approval source")}
          hint={t("Who decided and where. Approval allows reuse only; no document is issued or sent.")}
          required
          disabled={busy || !view.spec.calculation.confirmed}
        >
          {(control) => <input {...control} className="field w-full max-sm:min-h-11" name="template-approval-source" value={source} onChange={(event) => setSource(event.target.value)} />}
        </Field>}
      </>}
    </div>
  </DrawerShell> : null;

  useEffect(() => {
    if (!enabled || !canApprove) {
      setPublishedPendingTemplates(0);
    } else if (directory !== null) {
      setPublishedPendingTemplates(pendingCount);
    }
  }, [enabled, canApprove, directory, pendingCount]);

  return { rail, templateList, drawer, pendingCount: directory === null ? undefined : pendingCount, open, refresh };
}

function CalculationRules({ calculation }: { calculation: TemplateSpec["calculation"] }) {
  const { locale, t } = useLocale();
  const percent = new Intl.NumberFormat(locale === "zh-TW" ? "zh-HK" : "en", { style: "percent", maximumFractionDigits: 2 });
  const { currency, tax_rate_bps, discount_bps } = calculation;
  return <section aria-labelledby="template-review-calculation" className="space-y-2">
    <h4 id="template-review-calculation" className="font-medium text-ink-100">{t("Calculation rules")}</h4>
    <dl className="grid grid-cols-3 gap-3">
      <div><dt className="text-label text-ink-300">{t("Currency")}</dt><dd className="font-medium">{currency}</dd></div>
      <div><dt className="text-label text-ink-300">{t("Tax")}</dt><dd className="num font-medium">{percent.format(tax_rate_bps / 10_000)}</dd></div>
      <div><dt className="text-label text-ink-300">{t("Discount")}</dt><dd className="num font-medium">{percent.format(discount_bps / 10_000)}</dd></div>
    </dl>
  </section>;
}
