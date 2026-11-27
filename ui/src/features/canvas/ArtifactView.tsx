import { useEffect, useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Md, { CodeSurface } from "../../ui/Md";
import { artifactBytes } from "./artifactClient";
import { CSV_MAX_COLS, CSV_MAX_ROWS, parseCsv } from "./csv";
import type { ArtifactKind } from "./canvasStore";
import ImageOverlay from "./ImageOverlay";
import { parseDocxPreview, parsePptxPreview, type DocxPreview, type PptxPreview } from "./officePreview";
import WorkbookPreview from "./workbook/WorkbookPreview";

export interface Loaded {
  bytes: ArrayBuffer;
  mime: string;
}

/** How an HTML artifact is shown: in its sandboxed frame, or its source. */
export type HtmlView = "rendered" | "source";

/** The sandboxed document an HTML artifact renders in: an opaque origin
 *  (`sandbox=""` on the frame — never allow-same-origin), plus a policy
 *  that allows inline style and data images and nothing else. */
export function htmlFrameDocument(html: string): string {
  const csp =
    "default-src 'none'; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"><meta name="referrer" content="no-referrer"></head><body>${html}</body></html>`;
}

const EXT: Record<ArtifactKind, string> = {
  md: "md",
  pdf: "pdf",
  html: "html",
  image: "png",
  csv: "csv",
  docx: "docx",
  xlsx: "xlsx",
  pptx: "pptx",
};

const OFFICE_KIND_LABEL: Record<"docx" | "xlsx" | "pptx", string> = {
  docx: "Word document (.docx)",
  xlsx: "Excel workbook (.xlsx)",
  pptx: "PowerPoint deck (.pptx)",
};

export function downloadName(title: string, kind: ArtifactKind, mime: string): string {
  const cleaned = title
    .replace(/[\u0000-\u001f\u007f/\\:*?"<>|]+/g, "_")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+/, "")
    .slice(0, 80)
    .trim()
    .replace(/\.+$/, "");
  const base = cleaned || "artifact";
  const ext = kind === "image" ? (mime.split("/")[1]?.split("+")[0] ?? "png") : EXT[kind];
  return base.toLowerCase().endsWith(`.${ext.toLowerCase()}`) ? base : `${base}.${ext}`;
}

/** Hand bytes to the browser as a file. */
export function saveBytes(bytes: ArrayBuffer, mime: string, filename: string): void {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime.split(";")[0] }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Fetch one version's bytes through the board (operator session), aborting
 *  when the version in view changes. */
export function useArtifactBytes(conversation: string, id: string, version: number) {
  const [state, setState] = useState<
    { status: "loading" } | { status: "ready"; data: Loaded } | { status: "failed"; error: string }
  >({ status: "loading" });
  useEffect(() => {
    const abort = new AbortController();
    setState({ status: "loading" });
    artifactBytes(id, version, conversation, abort.signal)
      .then((data) => {
        if (!abort.signal.aborted) setState({ status: "ready", data });
      })
      .catch((e: unknown) => {
        if (!abort.signal.aborted) setState({ status: "failed", error: e instanceof Error ? e.message : "Could not load" });
      });
    return () => abort.abort();
  }, [conversation, id, version]);
  return state;
}

/** A `data:` URL for an image: the board CSP allows `data:` images and not
 *  `blob:` ones. SVG only ever shows as an `<img>` source, never as a URL. */
export function imageDataUrl(data: Loaded): string {
  let bin = "";
  const bytes = new Uint8Array(data.bytes);
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:${data.mime.split(";")[0]};base64,${btoa(bin)}`;
}

/** Open already-loaded PDF bytes in the browser's own top-level PDF viewer.
 *  Inline `<embed>`/`<iframe>` PDFs are blocked by the board's `object-src`,
 *  while a top-level `blob:` navigation opened from a click succeeds. */
export function openPdfInTab(bytes: ArrayBuffer): void {
  const tab = window.open("about:blank", "_blank");
  if (tab === null) return;
  let href: string | null = null;
  try {
    href = URL.createObjectURL(new Blob([bytes], { type: "application/pdf" }));
    tab.location.replace(href);
    const created = href;
    window.setTimeout(() => URL.revokeObjectURL(created), 30_000);
  } catch {
    if (href !== null) URL.revokeObjectURL(href);
    tab.close();
  }
}

function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const decode = (data: Loaded): string => new TextDecoder("utf-8").decode(data.bytes);

/** A CSV as a table: header row, sticky header, monospace cells, scrolling
 *  both ways; the first `CSV_MAX_ROWS` rows. Plain text when it will not parse. */
function CsvView({ text, contentRef }: { text: string; contentRef: React.RefObject<HTMLDivElement | null> }) {
  const t = useTranslate();
  const table = useMemo(() => parseCsv(text), [text]);
  if (!table) {
    return (
      <div ref={contentRef} className="canvas-doc min-w-0" data-artifact-view="csv">
        <pre className="num text-micro text-ink-200 whitespace-pre overflow-auto"
          role="region" aria-label={t("CSV text")} tabIndex={0}>{text}</pre>
      </div>
    );
  }
  return (
    <div ref={contentRef} className="canvas-doc cv-csv" data-artifact-view="csv" data-csv-table>
      <div className="cv-csv-scroll" role="region" aria-label={t("CSV table")} tabIndex={0}>
        <table>
          <thead>
            <tr>
              {table.header.map((h, i) => (
                <th key={i} scope="col" title={h}>{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.map((r, i) => (
              <tr key={i}>
                {r.map((c, j) => (
                  <td key={j} title={c}>{c}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {table.total > CSV_MAX_ROWS ? (
        <p className="num text-micro text-ink-500">
          {t("showing first")} {CSV_MAX_ROWS} {t("of")} {table.total}
        </p>
      ) : null}
      {table.columns > CSV_MAX_COLS ? (
        <p className="num text-micro text-ink-500">
          {t("showing first")} {CSV_MAX_COLS} {t("of")} {table.columns} {t("columns")}
        </p>
      ) : null}
    </div>
  );
}

/** Render one artifact version. Markdown through `Md`, CSV as a table, HTML in
 *  an inert frame, images inline, PDF in the browser's viewer with Download
 *  beside it where inline PDF is poor. */
export default function ArtifactView({
  kind,
  title,
  data,
  contentRef,
  htmlView = "rendered",
  focusablePreview = false,
  aspect,
  onOpenInWorks,
}: {
  kind: ArtifactKind;
  title: string;
  data: Loaded;
  contentRef: React.RefObject<HTMLDivElement | null>;
  htmlView?: HtmlView;
  /** Include the rendered document in an enclosing drawer's explicit tab order. */
  focusablePreview?: boolean;
  /** A generated image's shape, for its overlay. */
  aspect?: string;
  /** Open this Office file directly in an editable Works tab. */
  onOpenInWorks?: () => void;
}) {
  const t = useTranslate();
  switch (kind) {
    case "md":
      return (
        <div ref={contentRef} className="canvas-doc issue-reader text-secondary text-ink-200 break-words min-w-0" data-artifact-view="md">
          <Md text={decode(data)} focusableTables={focusablePreview} />
        </div>
      );
    case "csv":
      return <CsvView text={decode(data)} contentRef={contentRef} />;
    case "html": {
      if (htmlView === "source") {
        // ignoreBOM: the source shows every stored character, a leading BOM included.
        const source = new TextDecoder("utf-8", { ignoreBOM: true }).decode(data.bytes);
        return (
          <div ref={contentRef} className="canvas-doc cv-source" data-artifact-view="html-source">
            <CodeSurface label="html" code={source}>
              <code>{source}</code>
            </CodeSurface>
          </div>
        );
      }
      return (
        <iframe
          className="canvas-frame"
          data-artifact-view="html"
          title={title}
          tabIndex={focusablePreview ? 0 : undefined}
          sandbox=""
          referrerPolicy="no-referrer"
          srcDoc={htmlFrameDocument(decode(data))}
        />
      );
    }
    case "image":
      return (
        <div className="canvas-doc" data-artifact-view="image">
          <div className="imgshot" data-canvas-image>
            <img className="canvas-image" src={imageDataUrl(data)} alt={title} />
            <ImageOverlay
              title={title}
              aspect={aspect}
              mime={data.mime}
              bytes={data.bytes.byteLength}
              onDownload={() => saveBytes(data.bytes, data.mime, downloadName(title, kind, data.mime))}
            />
          </div>
        </div>
      );
    case "pdf":
      // No inline PDF: the board CSP allows no blob/object source, so the
      // drawer offers Open in new tab (top-level blob navigation) and Download.
      return (
        <div className="canvas-doc canvas-pdf" data-artifact-view="pdf">
          <p className="text-body text-ink-100">{title}</p>
          <p className="num text-micro text-ink-500">PDF · {sizeLabel(data.bytes.byteLength)}</p>
          <p className="text-micro text-ink-400">{t("A preview is not available yet. Open in a new tab or use Download.")}</p>
          <div className="flex flex-wrap items-center gap-3 pt-1">
            <button
              type="button"
              className="lnk text-micro"
              onClick={() => openPdfInTab(data.bytes)}
            >
              {t("Open in new tab")}
            </button>
          </div>
        </div>
      );
    case "xlsx":
      return (
        <div className="canvas-doc cv-office-preview" data-artifact-view="xlsx">
          <OfficePreviewBar
            title={title}
            kind="xlsx"
            data={data}
            onOpenInWorks={onOpenInWorks}
          />
          <WorkbookPreview bytes={data.bytes} title={title} />
        </div>
      );
    case "docx":
      return (
        <DocxInlineView
          title={title}
          data={data}
          contentRef={contentRef}
          onOpenInWorks={onOpenInWorks}
        />
      );
    case "pptx":
      return (
        <PptxInlineView
          title={title}
          data={data}
          contentRef={contentRef}
          onOpenInWorks={onOpenInWorks}
        />
      );
  }
}

function OfficePreviewBar({
  title,
  kind,
  data,
  onOpenInWorks,
}: {
  title: string;
  kind: "docx" | "xlsx" | "pptx";
  data: Loaded;
  onOpenInWorks?: () => void;
}) {
  const t = useTranslate();
  return (
    <div className="cv-office-bar">
      <div>
        <p className="text-body text-ink-100">{title}</p>
        <p className="num text-micro text-ink-500">
          {t(OFFICE_KIND_LABEL[kind])} · {sizeLabel(data.bytes.byteLength)} · {t("Read-only preview")}
        </p>
      </div>
      <div className="cv-office-actions">
        {onOpenInWorks ? (
          <button type="button" className="btn" onClick={onOpenInWorks}>
            {t("Open in Works")}
          </button>
        ) : null}
        <button
          type="button"
          className="btn btn-ghost"
          onClick={() => saveBytes(data.bytes, data.mime, downloadName(title, kind, data.mime))}
        >
          {t("Download")}
        </button>
      </div>
    </div>
  );
}

function OfficeFallbackCard({
  title,
  kind,
  data,
  message,
  onOpenInWorks,
}: {
  title: string;
  kind: "docx" | "pptx";
  data: Loaded;
  message: string;
  onOpenInWorks?: () => void;
}) {
  const t = useTranslate();
  return (
    <div className="canvas-doc canvas-pdf" data-artifact-view={kind}>
      <p className="text-body text-ink-100">{title}</p>
      <p className="num text-micro text-ink-500">{t(OFFICE_KIND_LABEL[kind])} · {sizeLabel(data.bytes.byteLength)}</p>
      <p className="text-micro text-ink-400">{message}</p>
      <div className="flex flex-wrap items-center gap-3 pt-1">
        {onOpenInWorks ? (
          <button type="button" className="btn" onClick={onOpenInWorks}>
            {t("Open in Works")}
          </button>
        ) : null}
        <button
          type="button"
          className="btn"
          onClick={() => saveBytes(data.bytes, data.mime, downloadName(title, kind, data.mime))}
        >
          {t("Download")}
        </button>
      </div>
    </div>
  );
}

function DocxInlineView({
  title,
  data,
  contentRef,
  onOpenInWorks,
}: {
  title: string;
  data: Loaded;
  contentRef: React.RefObject<HTMLDivElement | null>;
  onOpenInWorks?: () => void;
}) {
  const t = useTranslate();
  const [state, setState] = useState<
    { status: "loading" } | { status: "ready"; preview: DocxPreview } | { status: "error" }
  >({ status: "loading" });

  useEffect(() => {
    let active = true;
    setState({ status: "loading" });
    void parseDocxPreview(data.bytes).then(
      (preview) => { if (active) setState({ status: "ready", preview }); },
      () => { if (active) setState({ status: "error" }); },
    );
    return () => { active = false; };
  }, [data.bytes]);

  if (state.status === "loading") {
    return (
      <div className="canvas-doc cv-office-preview" data-artifact-view="docx">
        <OfficePreviewBar title={title} kind="docx" data={data} onOpenInWorks={onOpenInWorks} />
        <p className="text-micro text-ink-400 p-2" role="status">{t("Loading preview…")}</p>
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <OfficeFallbackCard
        title={title}
        kind="docx"
        data={data}
        message={t("Could not preview this file. Use Download to open locally.")}
        onOpenInWorks={onOpenInWorks}
      />
    );
  }
  return (
    <div ref={contentRef} className="canvas-doc cv-office-preview" data-artifact-view="docx">
      <OfficePreviewBar title={title} kind="docx" data={data} onOpenInWorks={onOpenInWorks} />
      {state.preview.blocks.length === 0 ? (
        <p className="text-micro text-ink-400 p-2">{t("This file has no previewable text yet.")}</p>
      ) : (
        <div className="cv-docx-sheet">
          {state.preview.blocks.map((block, idx) => {
            if (block.kind === "heading") {
              if (block.level === 1) return <h1 key={idx}>{block.text}</h1>;
              if (block.level === 2) return <h2 key={idx}>{block.text}</h2>;
              return <h3 key={idx}>{block.text}</h3>;
            }
            if (block.kind === "list_item") {
              return <div key={idx} className="cv-docx-li"><span aria-hidden>•</span><span>{block.text}</span></div>;
            }
            if (block.kind === "table") {
              return (
                <div key={idx} className="overflow-x-auto">
                  <table className="cv-docx-table">
                    <tbody>
                      {block.rows.map((row, r) => (
                        <tr key={r}>
                          {row.map((cell, c) => r === 0 ? <th key={c}>{cell}</th> : <td key={c}>{cell}</td>)}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              );
            }
            return <p key={idx}>{block.text}</p>;
          })}
          {state.preview.truncated ? (
            <p className="text-micro text-ink-500">{t("Preview truncated.")}</p>
          ) : null}
        </div>
      )}
    </div>
  );
}

function PptxInlineView({
  title,
  data,
  contentRef,
  onOpenInWorks,
}: {
  title: string;
  data: Loaded;
  contentRef: React.RefObject<HTMLDivElement | null>;
  onOpenInWorks?: () => void;
}) {
  const t = useTranslate();
  const [state, setState] = useState<
    { status: "loading" } | { status: "ready"; preview: PptxPreview } | { status: "error" }
  >({ status: "loading" });

  useEffect(() => {
    let active = true;
    setState({ status: "loading" });
    void parsePptxPreview(data.bytes).then(
      (preview) => { if (active) setState({ status: "ready", preview }); },
      () => { if (active) setState({ status: "error" }); },
    );
    return () => { active = false; };
  }, [data.bytes]);

  if (state.status === "loading") {
    return (
      <div className="canvas-doc cv-office-preview" data-artifact-view="pptx">
        <OfficePreviewBar title={title} kind="pptx" data={data} onOpenInWorks={onOpenInWorks} />
        <p className="text-micro text-ink-400 p-2" role="status">{t("Loading preview…")}</p>
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <OfficeFallbackCard
        title={title}
        kind="pptx"
        data={data}
        message={t("Could not preview this file. Use Download to open locally.")}
        onOpenInWorks={onOpenInWorks}
      />
    );
  }
  return (
    <div ref={contentRef} className="canvas-doc cv-office-preview" data-artifact-view="pptx">
      <OfficePreviewBar title={title} kind="pptx" data={data} onOpenInWorks={onOpenInWorks} />
      <div className="cv-pptx-deck">
        {state.preview.slides.map((slide) => (
          <section key={slide.index} className="cv-pptx-slide" aria-label={t("Slide {n}").replace("{n}", String(slide.index))}>
            <span className="num text-micro text-ink-500">{t("Slide {n}").replace("{n}", String(slide.index))}</span>
            {slide.title ? <h3 className="cv-pptx-slide-title">{slide.title}</h3> : null}
            {slide.lines.length > 0 ? (
              <ul className="cv-pptx-slide-lines">
                {slide.lines.map((line, i) => <li key={i}>{line}</li>)}
              </ul>
            ) : !slide.title ? (
              <p className="text-micro text-ink-500">{t("This file has no previewable text yet.")}</p>
            ) : null}
          </section>
        ))}
        {state.preview.truncated ? (
          <p className="text-micro text-ink-500">{t("Preview truncated.")}</p>
        ) : null}
      </div>
    </div>
  );
}
