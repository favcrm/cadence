import { useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import { onPrivateReadsPaused, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { handOffProposal, proposalRequest } from "./proposal";
import {
  base64Blob,
  docxBase64,
  templatesApi,
  TemplateError,
  type DraftView,
  type IntakeView,
  type SampleView,
  type TemplateList,
  type TemplateSummary,
} from "./templatesApi";

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
import "./templates.css";

/** Retain accepted bytes, not disposable handles, across same-session Activity pauses. */
interface SampleState {
  view: SampleView;
  docx: Blob;
  pdf: Blob;
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
/** The reader's findings for a DOCX: the facts it found, with locators. */
function DocxFindings({ evidence }: { evidence: Extract<IntakeView["provenance"]["evidence"], { format: "docx" }> }) {
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

export default function Templates() {
  const [name, setName] = useState("");
  const [docx, setDocx] = useState<File | null>(null);
  const [specText, setSpecText] = useState("");
  const [list, setList] = useState<TemplateList | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<DraftView | null>(null);
  const [draftVersion, setDraftVersion] = useState<number | null>(null);
  const [draftData, setDraftData] = useState("");
  const [approveSource, setApproveSource] = useState("");
  const [exported, setExported] = useState<{ id: string; docx: Blob; pdf: Blob; generation: number } | null>(null);
  const [reference, setReference] = useState<IntakeView | null>(null);
  const [sample, setSample] = useState<SampleState | null>(null);
  const [reconstruction, setReconstruction] = useState<{ blob: Blob; generation: number } | null>(null);
  const sampleDocxUrl = useBlobUrl(sample?.docx, sample?.generation);
  const samplePdfUrl = useBlobUrl(sample?.pdf, sample?.generation);
  const exportedDocxUrl = useBlobUrl(exported?.docx, exported?.generation);
  const exportedPdfUrl = useBlobUrl(exported?.pdf, exported?.generation);
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

  function parseJson(text: string, what: string): unknown {
    try {
      return JSON.parse(text);
    } catch {
      throw new Error(`${what} is not valid JSON`);
    }
  }

  function create() {
    return run(async (current) => {
      if (!docx) throw new Error("choose the company DOCX template first");
      const spec = parseJson(specText, "the spec");
      const bytes = await docx.arrayBuffer();
      if (!current()) return;
      const view = await templatesApi.create(name, docxBase64(bytes), spec);
      if (!current()) return;
      setMessage(`Created ${view.template.name} version ${view.template.version} as a draft. It is not approved.`);
      await refresh(name, current);
    });
  }

  function approve(version: TemplateSummary) {
    return run(async (current) => {
      if (!approveSource.trim()) throw new Error("say who decided and where before approving");
      const view = await templatesApi.approve(name, version.version, version.content_sha256, approveSource.trim());
      if (!current()) return;
      setMessage(`Approved ${view.template.name} version ${view.template.version}. Approved versions are immutable.`);
      await refresh(name, current);
    });
  }

  function createDocument(version: TemplateSummary) {
    return run(async (current) => {
      const data = parseJson(draftData, "the draft data");
      const created = await templatesApi.createDocument(name, version.version, data);
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
      const view = await templatesApi.sample(name, version.version);
      if (!current()) return;
      const docx = base64Blob(view.docx_base64, DOCX_TYPE);
      const pdf = base64Blob(view.pdf_base64, "application/pdf");
      setSample({ view, docx, pdf, generation: privateSessionGeneration() });
      setMessage(
        `Rendered a SAMPLE preview of ${view.status} v${version.version} with placeholder values. It is not a document.`,
      );
    });
  }

  function exportDocument(id: string) {
    return run(async (current) => {
      const view = await templatesApi.exportDocument(id);
      if (!current()) return;
      setExported({
        id: view.id,
        docx: base64Blob(view.docx_base64, "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        pdf: base64Blob(view.pdf_base64, "application/pdf"),
        generation: privateSessionGeneration(),
      });
      setMessage(`Rendered draft ${view.id} as DOCX and PDF. Nothing was issued or sent.`);
    });
  }

  return (
    <main className="templates-page" data-settings="templates">
      <header>
        <h1>Document templates</h1>
        <p className="templates-note">
          Drafts only. Creating a template or a draft issues no invoice, allocates no number and sends nothing.
          Approval covers the exact DOCX and calculation rules shown, not future commercial content.
        </p>
      </header>

      <section aria-labelledby="templates-reference">
        <h2 id="templates-reference">Read a reference document</h2>
        <p className="templates-note">
          Upload the company's existing quotation or invoice as DOCX, or a readable PDF (scanned PDFs are refused).
          The reader lists the text, placeholders, tables and styles it finds. It does not decide which parts are
          variable or what the rules are; you confirm those in the spec below.
        </p>
        <Button variant="secondary" loading={busy} disabled={!docx || busy} onClick={inspectReference}>
          Read reference
        </Button>
        {reference && (
          <div className="templates-reference" data-reference={reference.file}>
            <h3>Findings for {reference.file}</h3>
            {reference.provenance.evidence.format === "docx" ? (
              <DocxFindings evidence={reference.provenance.evidence} />
            ) : (
              <p>
                Read {reference.provenance.evidence.pages.length} PDF page(s) from the text layer. Layout, tables and
                images are not read.
              </p>
            )}
            {reference.reconstruction_base64 && reference.provenance.reconstruction && (
              <div className="templates-warning" role="note">
                <strong>Reference reconstruction, not an approved template.</strong>
                <ul>
                  {reference.provenance.reconstruction.limitations.map((limitation) => (
                    <li key={limitation}>{limitation}</li>
                  ))}
                </ul>
                <a
                  download={`${reference.file}.reconstruction.docx`}
                  href={reconstructionUrl}
                >
                  Download reconstruction DOCX
                </a>
              </div>
            )}
            <p className="templates-digest">
              <span>source {reference.provenance.source.sha256}</span>
            </p>
            <Button
              variant="secondary"
              onClick={() =>
                handOffProposal(proposalRequest(name || "new-template", reference.file, reference.provenance))
              }
            >
              Ask the agent to propose fields and questions
            </Button>
            <p className="templates-note">
              Opens the chat with the reader findings and the request, unsent. The agent proposes fields and
              questions and creates a draft only from your confirmed answers. It never approves. Send it yourself.
            </p>
          </div>
        )}
      </section>

      <section aria-labelledby="templates-new">
        <h2 id="templates-new">New template version</h2>
        <label>
          Name
          <input value={name} onChange={(event) => setName(event.target.value)} placeholder="quotation-acme" />
        </label>
        <label>
          Company DOCX
          <input type="file" accept=".docx" onChange={(event) => setDocx(event.target.files?.[0] ?? null)} />
        </label>
        <label>
          Spec (JSON: kind, fields, fixed_text, calculation)
          <textarea rows={8} value={specText} onChange={(event) => setSpecText(event.target.value)} />
        </label>
        <Button variant="primary" loading={busy} disabled={!name || !docx} onClick={create}>
          Create draft version
        </Button>
        <Button variant="secondary" disabled={!name || busy} onClick={() => run((current) => refresh(name, current))}>
          Show versions
        </Button>
      </section>

      {message && <p role="status" className="templates-message">{message}</p>}
      {error && <p role="alert" className="templates-error">{error}</p>}

      {list && (
        <section aria-labelledby="templates-versions">
          <h2 id="templates-versions">Versions of {list.name}</h2>
          {list.versions.length === 0 && <p>No versions yet.</p>}
          <ul className="templates-versions">
            {list.versions.map((version) => (
              <li key={version.version} data-version={version.version} data-status={version.status}>
                <strong>v{version.version}</strong> {version.status}
                {version.based_on !== null && <> from v{version.based_on}</>}
                <div className="templates-digest">
                  <span>content {version.content_sha256}</span>
                  <span>source {version.source_sha256}</span>
                </div>
                {version.status === "draft" && (
                  <div className="templates-actions">
                    <Button variant="secondary" disabled={busy} onClick={() => previewSample(version)}>
                      Preview sample v{version.version} before approval
                    </Button>
                  </div>
                )}
                {version.status === "draft" && (
                  <div className="templates-actions">
                    <label>
                      Approval source (who decided and where)
                      <input value={approveSource} onChange={(event) => setApproveSource(event.target.value)} />
                    </label>
                    <Button variant="primary" disabled={busy} onClick={() => approve(version)}>
                      Approve v{version.version} exactly
                    </Button>
                  </div>
                )}
                {version.status === "approved" && (
                  <div className="templates-actions">
                    <Button variant="secondary" disabled={busy} onClick={() => previewSample(version)}>
                      Preview sample v{version.version}
                    </Button>
                  </div>
                )}
                {version.status === "approved" && (
                  <div className="templates-actions">
                    <label>
                      Draft data (JSON: fields and items)
                      <textarea
                        rows={6}
                        value={draftVersion === version.version ? draftData : ""}
                        onFocus={() => setDraftVersion(version.version)}
                        onChange={(event) => {
                          setDraftVersion(version.version);
                          setDraftData(event.target.value);
                        }}
                      />
                    </label>
                    <Button variant="secondary" disabled={busy} onClick={() => createDocument(version)}>
                      Create draft from v{version.version}
                    </Button>
                  </div>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      {sample && sampleDocxUrl && samplePdfUrl && (
        <section aria-labelledby="templates-sample" data-sample={`${sample.view.template}-v${sample.view.version}`}>
          <h2 id="templates-sample">
            SAMPLE preview of {sample.view.template} v{sample.view.version} ({sample.view.status})
          </h2>
          <p role="note" className="templates-warning">
            SAMPLE: placeholder values only. This is not company data and not a document to send.
            {sample.view.status === "draft" && " This draft is not approved; approving it is a separate step."}
          </p>
          <iframe
            className="templates-pdf"
            src={samplePdfUrl}
            sandbox=""
            title={`SAMPLE PDF of ${sample.view.template} v${sample.view.version}`}
          />
          <p className="templates-actions">
            <a download={`${sample.view.template}-v${sample.view.version}-sample.docx`} href={sampleDocxUrl}>
              Download SAMPLE DOCX
            </a>{" "}
            <a download={`${sample.view.template}-v${sample.view.version}-sample.pdf`} href={samplePdfUrl}>
              Download SAMPLE PDF
            </a>
          </p>
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
          <h2 id="templates-draft">Draft {draft.id}</h2>
          <p className="templates-note">
            Sample or real data entered here. Totals are computed by the server from the confirmed rules.
          </p>
          <Button variant="secondary" disabled={busy} onClick={() => exportDocument(draft.id)}>
            Render DOCX and PDF preview
          </Button>
          {exported && exported.id === draft.id && exportedDocxUrl && exportedPdfUrl && (
            <p className="templates-actions">
              <a download={`${draft.id}.docx`} href={exportedDocxUrl}>Download DOCX</a>
              {" "}
              <a download={`${draft.id}.pdf`} href={exportedPdfUrl}>Download PDF</a>
            </p>
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
