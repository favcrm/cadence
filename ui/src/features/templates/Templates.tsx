import { useContext, useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { WriteGate } from "../auth/WriteGate";
import { htmlFrameDocument } from "../canvas/ArtifactView";
import { consumeCanvasTemplatePrefill, onCanvasTemplatePrefill } from "./canvasTemplateDraft";
import { handOffProposal, proposalRequest } from "./proposal";
import {
  base64Blob,
  docxBase64,
  templatesApi,
  TemplateError,
  type DraftView,
  type IntakeView,
  type SampleView,
  type TemplateDirectoryEntry,
  type TemplateList,
  type TemplateSummary,
} from "./templatesApi";

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
import "./templates.css";

/** Retain accepted bytes, not disposable handles, across same-session Activity pauses. */
interface SampleState {
  view: SampleView;
  docx?: Blob;
  pdf?: Blob;
  htmlBlob?: Blob;
  generation: number;
}

/** Each admitted effect owns its handle; Activity resume creates a fresh one. */
function useBlobUrl(blob: Blob | undefined, generation: number | undefined): string | undefined {
  const [loaded, setLoaded] = useState<{ blob: Blob; url: string } | null>(null);
  useEffect(() => {
    if (!blob || !privateReadsAllowed() || generation !== privateSessionGeneration()) return;
    const url = URL.createObjectURL(blob);
    let live = true;
    setLoaded({ blob, url });
    const dispose = () => {
      if (!live) return;
      live = false;
      URL.revokeObjectURL(url);
      setLoaded(null);
    };
    const stop = onPrivateReadsPaused(dispose);
    return () => {
      stop();
      dispose();
    };
  }, [blob, generation]);
  return loaded?.blob === blob && privateReadsAllowed() && generation === privateSessionGeneration()
    ? loaded?.url : undefined;
}

/**
 * Settings -> Templates (CAD-1393): company DOCX templates. The page only
 * relays the daemon; scope, the approval digest and every refusal are the
 * daemon's. Approval shows the exact digest first and is sent only for it.
 */
function DownloadLinks({ items }: { items: Array<[string, string | undefined, string]> }) {
  return (
    <p className="templates-actions">
      {items.filter((x): x is [string, string, string] => Boolean(x[1])).map(([f, u, l], i) => (
        <span key={f}>{i > 0 ? " " : ""}<a download={f} href={u}>{l}</a></span>
      ))}
    </p>
  );
}

function SandboxedHtml({ html, title }: { html: string; title: string }) {
  return <iframe className="templates-frame" sandbox="" referrerPolicy="no-referrer" srcDoc={htmlFrameDocument(html)} title={title} />;
}

function decodeRender(v: { docx_base64?: string; pdf_base64?: string; html?: string; html_base64?: string }) {
  const htmlText = v.html ?? (v.html_base64 ? new TextDecoder().decode(Uint8Array.from(atob(v.html_base64), (c) => c.charCodeAt(0))) : undefined);
  return {
    docx: v.docx_base64 ? base64Blob(v.docx_base64, DOCX_TYPE) : undefined,
    pdf: v.pdf_base64 ? base64Blob(v.pdf_base64, "application/pdf") : undefined,
    htmlBlob: htmlText !== undefined ? new Blob([htmlText], { type: "text/html" }) : undefined,
    htmlText,
    generation: privateSessionGeneration(),
  };
}

/** The reader's findings for a DOCX: the facts it found, with locators. */
function DocxFindings({ evidence }: { evidence: Extract<IntakeView["provenance"]["evidence"], { format: "docx" }> }) {
  const t = useTranslate();
  if (!("structure" in evidence)) {
    return (
      <div className="templates-findings">
        <p>{evidence.paragraphs.length} {t("paragraph(s)")}, {evidence.tables.length} {t("table(s)")}.</p>
        <h4>{t("Placeholders found")}</h4>
        {evidence.placeholders.length === 0 ? <p>{t("None. The reader found no {{ name }} placeholders; the template needs them before it can render data.")}</p> : <ul>{evidence.placeholders.map((n) => <li key={n}><code>{`{{ ${n} }}`}</code></li>)}</ul>}
        {(evidence.paragraphs.length > 0 || evidence.tables.length > 0) && (
          <>
            <h4>{t("Text found")}</h4>
            <ol className="templates-blocks">
              {evidence.paragraphs.map((text, idx) => <li key={`p-${idx}`}><span className="templates-digest">p[{idx + 1}]</span> {text}</li>)}
              {evidence.tables.map((rows, ti) => <li key={`t-${ti}`}><span className="templates-digest">table[{ti + 1}]</span> {rows.map((c) => c.join(" | ")).join(" ; ")}</li>)}
            </ol>
          </>
        )}
      </div>
    );
  }
  const structure = evidence.structure;
  return (
    <div className="templates-findings">
      <p>
        {structure.text_blocks} text block(s), {structure.tables} table(s), {structure.images} image(s),
        styles: {structure.styles.length ? structure.styles.join(", ") : "none"}.
      </p>
      <h4>Placeholders found</h4>
      {structure.placeholders.length === 0 ? (
        <p>None. The reader found no {"{{ name }}"} placeholders; the template needs them before it can render data.</p>
      ) : (
        <ul>
          {structure.placeholders.map((name) => (
            <li key={name}>
              <code>{`{{ ${name} }}`}</code>
              {structure.table_placeholders.includes(name) ? " (inside a table)" : ""}
            </li>
          ))}
        </ul>
      )}
      {structure.loops.length > 0 && (
        <>
          <h4>Loop and condition tags</h4>
          <ul>
            {structure.loops.map((loop) => (
              <li key={`${loop.locator}-${loop.tag}`}>
                <code>{`{% ${loop.tag} %}`}</code> at {loop.locator}
              </li>
            ))}
          </ul>
        </>
      )}
      {structure.unrecognized_markers.length > 0 && (
        <div className="templates-warning" role="note">
          <strong>Unrecognized braces or tags, to resolve before approval:</strong>
          <ul>
            {structure.unrecognized_markers.map((marker) => (
              <li key={`${marker.locator}-${marker.text}`}>
                {marker.locator}: {marker.text}
              </li>
            ))}
          </ul>
        </div>
      )}
      <h4>Text found</h4>
      <ol className="templates-blocks">
        {evidence.blocks.map((block) => (
          <li key={`${block.locator}-${block.text.slice(0, 20)}`}>
            <span className="templates-digest">{block.locator}</span> {block.text}
          </li>
        ))}
      </ol>
    </div>
  );
}

export default function Templates({ viewer }: { viewer?: { operator: boolean | null; hosted?: boolean } }) {
  const t = useTranslate();
  const writeBlockReason = useContext(WriteGate);
  const readOnly = writeBlockReason !== null;
  const isHostedMember = Boolean(viewer?.hosted && viewer.operator === false);
  const canWriteTemplates = !readOnly && !isHostedMember;
  const [initPre] = useState(() => consumeCanvasTemplatePrefill());
  const [directory, setDirectory] = useState<TemplateDirectoryEntry[] | null>(null);
  const [name, setName] = useState(initPre?.name ?? "");
  const [baseVersion, setBaseVersion] = useState<number | null>(null);
  const [sourceMode, setSourceMode] = useState<"docx" | "html">(initPre ? "html" : "docx");
  const [htmlText, setHtmlText] = useState(initPre?.html ?? "");
  const [docx, setDocx] = useState<File | null>(null);
  const [specText, setSpecText] = useState(initPre?.specJson ?? "");
  const [list, setList] = useState<TemplateList | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<DraftView | null>(null);
  const [draftVersion, setDraftVersion] = useState<number | null>(null);
  const [draftData, setDraftData] = useState("");
  const [allowPartial, setAllowPartial] = useState(false);
  const [approveSource, setApproveSource] = useState("");
  const [exported, setExported] = useState<{ id: string; docx?: Blob; pdf?: Blob; htmlBlob?: Blob; htmlText?: string; generation: number } | null>(null);
  const [reference, setReference] = useState<IntakeView | null>(null);
  const [sample, setSample] = useState<SampleState | null>(null);
  const [reconstruction, setReconstruction] = useState<{ blob: Blob; generation: number } | null>(null);
  const sampleDocxUrl = useBlobUrl(sample?.docx, sample?.generation);
  const samplePdfUrl = useBlobUrl(sample?.pdf, sample?.generation);
  const sampleHtmlUrl = useBlobUrl(sample?.htmlBlob, sample?.generation);
  const exportedDocxUrl = useBlobUrl(exported?.docx, exported?.generation);
  const exportedPdfUrl = useBlobUrl(exported?.pdf, exported?.generation);
  const exportedHtmlUrl = useBlobUrl(exported?.htmlBlob, exported?.generation);
  const reconstructionUrl = useBlobUrl(reconstruction?.blob, reconstruction?.generation);
  const lifecycle = useRef(0);

  // Invalidating on both pause and cleanup also rejects an old decoded body
  // that finishes only after the same session has already been readmitted.
  useEffect(() => {
    const pause = () => {
      lifecycle.current += 1;
      setBusy(false);
    };
    const stop = onPrivateReadsPaused(pause);
    return () => {
      stop();
      pause();
    };
  }, []);

  const applyDraft = (n: string, b: number | null, s: string, h?: string) => {
    setBaseVersion(b); setName(n); setSpecText(s);
    if (h !== undefined) { setSourceMode("html"); setHtmlText(h); }
  };

  useEffect(() => {
    const apply = () => { const n = consumeCanvasTemplatePrefill(); if (n) applyDraft(n.name, null, n.specJson, n.html); };
    apply();
    return onCanvasTemplatePrefill(apply);
  }, []);

  async function run(action: (current: () => boolean) => Promise<void>) {
    if (!privateReadsAllowed()) return;
    const started = lifecycle.current;
    const generation = privateSessionGeneration();
    const current = () => privateReadsAllowed() && started === lifecycle.current
      && generation === privateSessionGeneration();
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await action(current);
    } catch (cause) {
      if (current()) setError(cause instanceof TemplateError || cause instanceof Error ? cause.message : "request failed");
    } finally {
      if (current()) setBusy(false);
    }
  }

  async function refresh(target: string, current: () => boolean) {
    if (!target) return;
    const view = await templatesApi.list(target);
    if (current()) setList(view);
  }

  const browseAll = () => run(async (c) => { const r = await templatesApi.listAll(); if (c()) setDirectory(r.templates); });

  function parseJson(text: string, what: string): unknown {
    try {
      return JSON.parse(text);
    } catch {
      throw new Error(`${what} is not valid JSON`);
    }
  }

  const requireWritable = () => { if (!canWriteTemplates) throw new Error(writeBlockReason ?? t("Only the workspace owner can create, revise, or approve templates on a hosted workspace.")); };

  function create() {
    return run(async (current) => {
      requireWritable();
      const spec = parseJson(specText, "the spec"), base = baseVersion;
      let view;
      if (sourceMode === "html") {
        if (!htmlText.trim()) throw new Error("enter or upload HTML template source first");
        view = base !== null ? await templatesApi.reviseHtml(name, base, htmlText, spec) : await templatesApi.createHtml(name, htmlText, spec);
      } else {
        if (!docx) throw new Error("choose the company DOCX template first");
        const bytes = await docx.arrayBuffer();
        if (!current()) throw new Error("session paused");
        view = base !== null ? await templatesApi.revise(name, base, docxBase64(bytes), spec) : await templatesApi.create(name, docxBase64(bytes), spec);
      }
      if (!current()) return;
      setBaseVersion(null);
      setMessage(`Created ${view.template.name} version ${view.template.version} as a draft. It is not approved.`);
      await refresh(name, current);
    });
  }

  function loadForRevision(version: TemplateSummary) {
    return run(async (current) => {
      const full = await templatesApi.version(list?.name || name, version.version);
      if (!current()) return;
      applyDraft(full.template.name, full.template.version, JSON.stringify(full.spec, null, 2), full.html);
      if (full.html === undefined && full.template.source_format === "docx") setSourceMode("docx");
      setMessage(`Loaded ${full.template.name} v${full.template.version} for revision.`);
    });
  }

  function approve(version: TemplateSummary) {
    return run(async (current) => {
      requireWritable();
      if (!approveSource.trim()) throw new Error("say who decided and where before approving");
      const view = await templatesApi.approve(name, version.version, version.content_sha256, approveSource.trim());
      if (!current()) return;
      setMessage(`Approved ${view.template.name} version ${view.template.version}. Approved versions are immutable.`);
      await refresh(name, current);
    });
  }

  function createDocument(version: TemplateSummary) {
    return run(async (current) => {
      requireWritable();
      const created = await templatesApi.createDocument(name, version.version, parseJson(draftData, "the draft data"), allowPartial);
      if (!current()) return;
      setDraft(created);
      setMessage(`Draft ${created.id} created from version ${version.version}. Nothing was issued or sent.`);
    });
  }

  function inspectReference() {
    return run(async (current) => {
      if (!docx) throw new Error("choose the reference DOCX or PDF first");
      const bytes = await docx.arrayBuffer();
      if (!current()) return;
      const view = await templatesApi.intake(docx.name, docxBase64(bytes));
      if (!current()) return;
      setReconstruction(view.reconstruction_base64 ? {
        blob: base64Blob(view.reconstruction_base64, DOCX_TYPE),
        generation: privateSessionGeneration(),
      } : null);
      setReference(view);
      setMessage(`Read ${view.file}. Nothing was stored. Review the findings below before writing a spec.`);
    });
  }

  function previewSample(version: TemplateSummary) {
    return run(async (current) => {
      const view = await templatesApi.sample(name, version.version, version.source_format === "html" ? "html" : undefined);
      if (!current()) return;
      setSample({ view, ...decodeRender(view) });
      setMessage(
        `Rendered a SAMPLE preview of ${view.status} v${version.version} with placeholder values. It is not a document.`,
      );
    });
  }

  function exportDocument(id: string, format?: "html" | "pdf") {
    return run(async (current) => {
      const view = await templatesApi.exportDocument(id, format);
      if (!current()) return;
      setExported({ id: view.id, ...decodeRender(view) });
      setMessage(`Rendered draft ${view.id} as ${view.docx_base64 ? "DOCX and PDF" : format === "pdf" ? "PDF" : "HTML"}. Nothing was issued or sent.`);
    });
  }

  const draftIsHtml = draft && list ? list.versions.find((v) => v.version === draft.version)?.source_format === "html" : false;
  const draftHasUnresolved = Boolean(draft?.has_unresolved_pricing || draft?.context.has_unresolved_pricing === true);

  return (
    <main className="templates-page" data-settings="templates">
      <header>
        <h1>{t("Document templates")}</h1>
        <p className="templates-note">{t("Drafts only. Creating a template or a draft issues no invoice, allocates no number and sends nothing. Approval covers the exact template content hash and calculation rules shown, not future commercial content.")}</p>
        {(readOnly || isHostedMember) && (
          <p role="note" className="templates-warning" data-templates-readonly>
            {readOnly ? writeBlockReason : t("Only the workspace owner can create, revise, or approve templates on a hosted workspace.")}
          </p>
        )}
      </header>

      <section aria-labelledby="templates-directory">
        <div className="templates-row">
          <h2 id="templates-directory">{t("Template directory")}</h2>
          <Button variant="secondary" loading={busy} disabled={busy} onClick={browseAll}>{t("Browse all templates")}</Button>
        </div>
        {directory && (directory.length === 0 ? <p className="templates-note">{t("No templates stored yet.")}</p> : (
          <ul className="templates-directory-list">
            {directory.map((entry) => {
              const av = entry.approved_version ?? entry.latest_approved_version ?? null;
              return (
                <li key={entry.name}>
                  <button type="button" className="templates-dir-item" onClick={() => { setName(entry.name); setBaseVersion(null); void run((c) => refresh(entry.name, c)); }}>
                    <strong>{entry.name}</strong> · {entry.kind} · {entry.source_format} · v{entry.latest_version}{av !== null ? ` (${t("approved")} v${av})` : ` (${t("draft")})`}
                  </button>
                </li>
              );
            })}
          </ul>
        ))}
      </section>

      <section aria-labelledby="templates-reference">
        <h2 id="templates-reference">{t("Read a reference document")}</h2>
        <p className="templates-note">{t("Upload the company’s existing quotation or invoice as DOCX, or a readable PDF (scanned PDFs are refused). The reader lists the text, placeholders, tables and styles it finds. It does not decide which parts are variable or what the rules are; you confirm those in the spec below.")}</p>
        <Button variant="secondary" loading={busy} disabled={!canWriteTemplates || !docx || busy} onClick={inspectReference}>{t("Read reference")}</Button>
        {reference && (
          <div className="templates-reference" data-reference={reference.file}>
            <h3>{t("Findings for")} {reference.file}</h3>
            {reference.provenance.evidence.format === "docx" ? <DocxFindings evidence={reference.provenance.evidence} /> : <p>{t("Read PDF pages from the text layer")}: {reference.provenance.evidence.pages.length}. {t("Layout, tables and images are not read.")}</p>}
            {reference.suggested_html && (
              <div className="templates-actions">
                <Button variant="secondary" onClick={() => { setBaseVersion(null); setSourceMode("html"); setHtmlText(reference.suggested_html ?? ""); setMessage(`Loaded suggested HTML from ${reference.file} into the template editor below.`); }}>{t("Use suggested HTML")}</Button>
              </div>
            )}
            {reference.reconstruction_base64 && reference.provenance.reconstruction && (
              <div className="templates-warning" role="note">
                <strong>{t("Reference reconstruction, not an approved template.")}</strong>
                <ul>{reference.provenance.reconstruction.limitations.map((l) => <li key={l}>{l}</li>)}</ul>
                <a download={`${reference.file}.reconstruction.docx`} href={reconstructionUrl}>{t("Download reconstruction DOCX")}</a>
              </div>
            )}
            <p className="templates-digest"><span>source {reference.provenance.source.sha256}</span></p>
            <Button variant="secondary" onClick={() => handOffProposal(proposalRequest(name || "new-template", reference.file, reference.provenance))}>{t("Ask the agent to propose fields and questions")}</Button>
            <p className="templates-note">{t("Opens the chat with the reader findings and the request, unsent. The agent proposes fields and questions and creates a draft only from your confirmed answers. It never approves. Send it yourself.")}</p>
          </div>
        )}
      </section>

      <section aria-labelledby="templates-new">
        <h2 id="templates-new">{baseVersion !== null ? `${t("Revise template")} v${baseVersion}` : t("New template version")}</h2>
        {baseVersion !== null && (
          <div className="templates-row" role="note" data-revision-base={baseVersion}>
            <span>{t("Revising from")} v{baseVersion}</span>
            <Button variant="secondary" disabled={busy} onClick={() => setBaseVersion(null)}>{t("Clear revision base")}</Button>
          </div>
        )}
        <div className="templates-mode" role="group" aria-label={t("Source format")}>
          <span>{t("Source format")}:</span>
          <button type="button" aria-pressed={sourceMode === "html"} onClick={() => setSourceMode("html")}>{t("HTML5 + print CSS")}</button>
          <button type="button" aria-pressed={sourceMode === "docx"} onClick={() => setSourceMode("docx")}>{t("Legacy DOCX")}</button>
        </div>
        <label>{t("Name")}<input value={name} onChange={(e) => { setName(e.target.value); if (baseVersion !== null) setBaseVersion(null); }} placeholder="quotation-acme" /></label>
        <label>{sourceMode === "html" ? t("Reference file (optional DOCX / PDF)") : t("Company DOCX")}<input type="file" accept=".docx,.pdf" onChange={(e) => setDocx(e.target.files?.[0] ?? null)} /></label>
        <label>{t("Spec (JSON: kind, fields, fixed_text, calculation)")}<textarea rows={8} value={specText} onChange={(e) => setSpecText(e.target.value)} /></label>
        {sourceMode === "html" && <label>{t("HTML template source (HTML5 + print CSS)")}<textarea rows={10} value={htmlText} onChange={(e) => setHtmlText(e.target.value)} placeholder="<!doctype html><html>...</html>" /></label>}
        <Button variant="primary" loading={busy} disabled={!canWriteTemplates || !name || (sourceMode === "html" ? !htmlText.trim() : !docx)} onClick={create}>{baseVersion !== null ? t("Create revision draft") : t("Create draft version")}</Button>
        <Button variant="secondary" disabled={!name || busy} onClick={() => run((current) => refresh(name, current))}>{t("Show versions")}</Button>
      </section>

      {message && <p role="status" className="templates-message">{message}</p>}
      {error && <p role="alert" className="templates-error">{error}</p>}

      {list && (
        <section aria-labelledby="templates-versions">
          <h2 id="templates-versions">{t("Versions of")} {list.name}</h2>
          {list.versions.length === 0 && <p>{t("No versions yet.")}</p>}
          <ul className="templates-versions">
            {list.versions.map((version) => (
              <li key={version.version} data-version={version.version} data-status={version.status}>
                <strong>v{version.version}</strong> {version.status}{version.source_format ? ` · ${version.source_format}` : ""}
                {version.based_on !== null && <> from v{version.based_on}</>}
                <div className="templates-digest"><span>content {version.content_sha256}</span><span>source {version.source_sha256}</span></div>
                <div className="templates-actions"><Button variant="secondary" disabled={!canWriteTemplates || busy} onClick={() => loadForRevision(version)}>{t("Load to revise")} v{version.version}</Button></div>
                {version.status === "draft" && (
                  <>
                    <div className="templates-actions"><Button variant="secondary" disabled={busy} onClick={() => previewSample(version)}>{t("Preview sample")} v{version.version} {t("before approval")}</Button></div>
                    <div className="templates-approval-box">
                      <p className="templates-note">{t("Owner approval binds exact content digest")} <code>{version.content_sha256}</code> {t("and calculation rules.")}</p>
                      <label>{t("Approval source (who decided and where)")}<input value={approveSource} onChange={(e) => setApproveSource(e.target.value)} /></label>
                      <Button variant="primary" disabled={!canWriteTemplates || busy} onClick={() => approve(version)}>{t("Approve")} v{version.version} {t("exactly")}</Button>
                    </div>
                  </>
                )}
                {version.status === "approved" && (
                  <>
                    <div className="templates-actions"><Button variant="secondary" disabled={busy} onClick={() => previewSample(version)}>{t("Preview sample")} v{version.version}</Button></div>
                    <div className="templates-actions">
                      <label>{t("Draft data (JSON: fields and items)")}<textarea rows={6} value={draftVersion === version.version ? draftData : ""} onFocus={() => setDraftVersion(version.version)} onChange={(e) => { setDraftVersion(version.version); setDraftData(e.target.value); }} /></label>
                      <label className="templates-checkbox"><input type="checkbox" checked={allowPartial} onChange={(e) => setAllowPartial(e.target.checked)} />{t("Allow unresolved pricing (UNRESOLVED)")}</label>
                      <Button variant="secondary" disabled={!canWriteTemplates || busy} onClick={() => createDocument(version)}>{t("Create draft from")} v{version.version}</Button>
                    </div>
                  </>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      {sample && (sampleHtmlUrl || (sampleDocxUrl && samplePdfUrl)) && (
        <section aria-labelledby="templates-sample" data-sample={`${sample.view.template}-v${sample.view.version}`}>
          <h2 id="templates-sample">
            SAMPLE preview of {sample.view.template} v{sample.view.version} ({sample.view.status})
          </h2>
          <p role="note" className="templates-warning">
            {t("SAMPLE: placeholder values only. This is not company data and not a document to send.")}
            {sample.view.status === "draft" && ` ${t("This draft is not approved; approving it is a separate step.")}`}
          </p>
          {sample.view.html !== undefined ? (
            <SandboxedHtml html={sample.view.html} title={`SAMPLE HTML of ${sample.view.template} v${sample.view.version}`} />
          ) : samplePdfUrl ? (
            <iframe
              className="templates-pdf"
              src={samplePdfUrl}
              sandbox=""
              title={`SAMPLE PDF of ${sample.view.template} v${sample.view.version}`}
            />
          ) : null}
          <DownloadLinks items={[
            [`${sample.view.template}-v${sample.view.version}-sample.html`, sampleHtmlUrl, `${t("Download")} SAMPLE HTML`],
            [`${sample.view.template}-v${sample.view.version}-sample.docx`, sampleDocxUrl, `${t("Download")} SAMPLE DOCX`],
            [`${sample.view.template}-v${sample.view.version}-sample.pdf`, samplePdfUrl, `${t("Download")} SAMPLE PDF`],
          ]} />
          <dl>
            {Object.entries(sample.view.context)
              .filter(([key]) => key !== "items")
              .map(([key, value]) => (
                <div key={key}>
                  <dt>{key}</dt>
                  <dd>{typeof value === "string" || typeof value === "number" ? String(value) : ""}</dd>
                </div>
              ))}
          </dl>
        </section>
      )}

      {draft && (
        <section aria-labelledby="templates-draft" data-draft={draft.id}>
          <h2 id="templates-draft">{t("Draft")} {draft.id}</h2>
          {draftHasUnresolved && <p role="note" className="templates-warning">{t("Contains UNRESOLVED pricing — review unit prices before finalizing.")}</p>}
          <p className="templates-note">{t("Sample or real data entered here. Totals are computed by the server from the confirmed rules.")}</p>
          {draftIsHtml ? (
            <div className="templates-row">
              <Button variant="secondary" disabled={busy} onClick={() => exportDocument(draft.id, "html")}>{t("Export HTML preview")}</Button>
              <Button variant="secondary" disabled={busy} onClick={() => exportDocument(draft.id, "pdf")}>{t("Export PDF preview")}</Button>
            </div>
          ) : (
            <Button variant="secondary" disabled={busy} onClick={() => exportDocument(draft.id)}>{t("Render DOCX and PDF preview")}</Button>
          )}
          {exported && exported.id === draft.id && (exportedHtmlUrl || exportedPdfUrl || exportedDocxUrl) && (
            <div className="templates-actions">
              {exported.htmlText !== undefined && <SandboxedHtml html={exported.htmlText} title={`Exported HTML for ${draft.id}`} />}
              <DownloadLinks items={[
                [`${draft.id}.html`, exportedHtmlUrl, `${t("Download")} HTML`],
                [`${draft.id}.docx`, exportedDocxUrl, `${t("Download")} DOCX`],
                [`${draft.id}.pdf`, exportedPdfUrl, `${t("Download")} PDF`],
              ]} />
            </div>
          )}
          <dl>
            {Object.entries(draft.context)
              .filter(([key]) => key !== "items")
              .map(([key, value]) => (
                <div key={key}>
                  <dt>{key}</dt>
                  <dd>{typeof value === "string" || typeof value === "number" ? String(value) : ""}</dd>
                </div>
              ))}
          </dl>
        </section>
      )}
    </main>
  );
}
