import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useLocale, useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import Button from "../../ui/Button";
import DrawerShell from "../app-shell/shared/DrawerShell";
import Field from "../app-shell/shared/Field";
import { htmlFrameDocument } from "../canvas/ArtifactView";
import { base64Blob, templatesApi, type TemplateDirectoryEntry, type TemplateSpec, type TemplateSummary, type TemplateView } from "./templatesApi";

export type HostedTemplateReviewTarget = { name: string; version: number; digest: string };
export type HostedTemplateRail = (onReview: () => void) => ReactNode;

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

/** One Home-owned review surface. Every continuation is bound to its selection and private session. */
export function useHostedTemplateReview({ enabled, canApprove, onCompose }: {
  enabled: boolean;
  canApprove: boolean;
  onCompose?: (text: string) => void;
}): { rail: HostedTemplateRail | undefined; drawer: ReactNode; pendingCount: number | undefined; open: (target: HostedTemplateReviewTarget, opener?: HTMLElement) => void; refresh: () => void } {
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
      setPreviewError(cause instanceof Error && cause.message === "receipt" ? "The sample receipt did not match the selected version, digest and row count. No preview is shown." : format === "pdf" ? "SAMPLE PDF is unavailable for this version. You can request a short HTML preview if supported." : "SAMPLE HTML is unavailable for this version.");
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

  const { byName, relevant, pendingCount } = useMemo(() => {
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
    return { byName, relevant: [...pending, ...approved], pendingCount: pending.length };
  }, [directory, canApprove]);
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
    kind="template-review" label={t("Template review")} kicker={t("Template approvals")} scrim="board" modal trapFocus
    title={`${target.name} · v${target.version}`} subtitle={<code className="break-all">SHA-256 {target.digest}</code>}
    onClose={close}
    tabs={[{ id: "review", label: t("Review"), panel: <div className="space-y-4">
      {loading && <p role="status">{t("Loading the exact template version…")}</p>}
      {error && <p role="alert" className="text-fail break-words">{t(error)} {detail}</p>}
      {error && !view && !busy && !loading && <Button size="sm" variant="secondary" onClick={() => open(target, openerRef.current)}>{t("Reload exact version")}</Button>}
      {notice && <p role="status">{t(notice)}</p>}
      {view && <>
        <p><strong>{t("Status")}:</strong> {view.template.status === "approved" ? t("Approved for reuse") : t("Draft awaiting review")}</p>
        <p className="text-label text-ink-400">{t("Approval covers this exact template and its rules for future drafts. It does not approve, issue or send a finished document.")}</p>
        {view.template.based_on !== null && <p>{t("Based on")} v{view.template.based_on}</p>}
        {view.template.approved_source && <p className="break-words"><strong>{t("Approval source")}:</strong> {view.template.approved_source}</p>}
        <TemplateRules spec={view.spec} />
        {history.length > 1 && <details><summary tabIndex={0}>{t("Version history")}</summary><ul className="mt-1 grid gap-1">{history.map((version) => <li key={version.version}>{version.version === target.version
          ? <span aria-current="true">v{version.version} · {t(version.status === "approved" ? "Approved" : "Draft")}</span>
          : <button type="button" disabled={busy} className="lnk" onClick={(event) => open(targetOf(version), event.currentTarget)}>v{version.version} · {t(version.status === "approved" ? "Approved" : "Draft")}</button>}</li>)}</ul></details>}
        <section aria-label={t("SAMPLE preview")}>
          <h4>{t("SAMPLE preview")}</h4>
          <p className="text-label text-ink-400">{t("Placeholder values only; this is not a document to send.")}</p>
          <div className="flex flex-wrap gap-2" role="group" aria-label={t("Sample length")}>
            <Button size="sm" variant="secondary" aria-pressed={rows === 1} disabled={previewBusy || busy} onClick={() => void loadSample(target, 1, "pdf", selection.current, privateSessionGeneration())}>{t("Short sample")}</Button>
            <Button size="sm" variant="secondary" aria-pressed={rows === 36} disabled={previewBusy || busy} onClick={() => void loadSample(target, 36, "pdf", selection.current, privateSessionGeneration())}>{t("Continuation sample (36 rows)")}</Button>
          </div>
          {previewBusy && <p role="status">{t("Rendering SAMPLE…")}</p>}
          {previewError && <p role="alert" className="text-warn break-words">{t(previewError)} {previewDetail}</p>}
          {previewError && <Button size="sm" variant="secondary" disabled={previewBusy || busy} onClick={() => void loadSample(target, 1, "html", selection.current, privateSessionGeneration())}>{t("Request short HTML preview")}</Button>}
          {preview?.kind === "pdf" && <div className="flex flex-wrap gap-3 pt-2">
            <a className="lnk" href={preview.url} target="_blank" rel="noopener noreferrer">{t("Open SAMPLE PDF in a new tab")}</a>
            <a className="lnk" href={preview.url} download={`${target.name}-v${target.version}-sample.pdf`}>{t("Download SAMPLE PDF")}</a>
          </div>}
          {preview?.kind === "html" && <iframe title={`${t("SAMPLE HTML")} · ${target.name} v${target.version}`} srcDoc={htmlFrameDocument(preview.html)} sandbox="" referrerPolicy="no-referrer" className="w-full min-h-96" />}
          {preview && <p className="text-micro text-ink-400">{t("Verified sample rows")}: {preview.rows}</p>}
        </section>
        {needsApproval && !view.spec.calculation.confirmed && <p className="text-warn">{t("Confirm the calculation rules in a new draft before approving.")}</p>}
        {needsApproval && <Field
          label={t("Approval source (who decided and where)")}
          hint={t("Record your decision and where it was made; this is not approval of a finished document.")}
          required
          disabled={busy || !view.spec.calculation.confirmed}
        >
          {(control) => <input {...control} className="field w-full" name="template-approval-source" value={source} onChange={(event) => setSource(event.target.value)} />}
        </Field>}
      </>}
    </div> }]}
    foot={view && (onCompose || needsApproval) ? <div className="crm-drawer-actions flex flex-wrap gap-2">
      {onCompose && <Button size="sm" variant="secondary" disabled={busy} onClick={() => compose("revise")}>{t("Ask agent to revise")}</Button>}
      {onCompose && view.template.status === "approved" && <Button size="sm" variant="primary" disabled={busy} onClick={() => compose("use")}>{t("Use template")}</Button>}
      {needsApproval && <Button size="sm" variant="primary" loading={busy} disabled={busy || !source.trim() || !view.spec.calculation.confirmed} onClick={() => void approve()}>{t("Approve")} v{target.version} {t("for reuse")}</Button>}
    </div> : undefined}
  /> : null;

  return { rail, drawer, pendingCount: directory === null ? undefined : pendingCount, open, refresh };
}

function TemplateRules({ spec }: { spec: TemplateSpec }) {
  const { locale, t } = useLocale();
  const percent = new Intl.NumberFormat(locale === "zh-TW" ? "zh-HK" : "en", { style: "percent", maximumFractionDigits: 2 });
  const { currency, tax_rate_bps, discount_bps } = spec.calculation;
  return <section aria-label={t("Schema and calculation rules")} className="space-y-2">
    <h4 className="font-medium">{t("Schema and calculation rules")}</h4>
    <dl className="grid grid-cols-3 gap-2 text-label">
      <div><dt className="text-ink-400">{t("Currency")}</dt><dd>{currency}</dd></div>
      <div><dt className="text-ink-400">{t("Tax")}</dt><dd>{percent.format(tax_rate_bps / 10_000)}</dd></div>
      <div><dt className="text-ink-400">{t("Discount")}</dt><dd>{percent.format(discount_bps / 10_000)}</dd></div>
    </dl>
    <ul className="grid gap-1 text-label">
      {spec.fields.map((field) => <li className="break-words" key={field.key}>
        {field.label} <code className="text-ink-400">{field.key}</code> · {t(field.kind)}{field.required ? ` · ${t("Required")}` : ""}
      </li>)}
    </ul>
  </section>;
}
