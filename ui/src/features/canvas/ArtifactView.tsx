import { useEffect, useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Md from "../../ui/Md";
import { artifactBytes } from "./artifactClient";
import { CSV_MAX_ROWS, parseCsv } from "./csv";
import type { ArtifactKind } from "./canvasStore";

export interface Loaded {
  bytes: ArrayBuffer;
  mime: string;
}

/** The sandboxed document an HTML artifact renders in: an opaque origin
 *  (`sandbox=""` on the frame — never allow-same-origin), plus a policy
 *  that allows inline style and data images and nothing else. */
export function htmlFrameDocument(html: string): string {
  const csp =
    "default-src 'none'; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"><meta name="referrer" content="no-referrer"></head><body>${html}</body></html>`;
}

const EXT: Record<ArtifactKind, string> = { md: "md", pdf: "pdf", html: "html", image: "png", csv: "csv" };

export function downloadName(title: string, kind: ArtifactKind, mime: string): string {
  const base = title.replace(/[^\w.\- ]+/g, "_").trim().slice(0, 80) || "artifact";
  const ext = kind === "image" ? (mime.split("/")[1]?.split("+")[0] ?? "png") : EXT[kind];
  return `${base}.${ext}`;
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
function imageDataUrl(data: Loaded): string {
  let bin = "";
  const bytes = new Uint8Array(data.bytes);
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:${data.mime.split(";")[0]};base64,${btoa(bin)}`;
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
        <pre className="num text-micro text-ink-200 whitespace-pre overflow-auto">{text}</pre>
      </div>
    );
  }
  return (
    <div ref={contentRef} className="canvas-doc cv-csv" data-artifact-view="csv" data-csv-table>
      <div className="cv-csv-scroll">
        <table>
          <thead>
            <tr>
              {table.header.map((h, i) => (
                <th key={i} scope="col">{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.map((r, i) => (
              <tr key={i}>
                {r.map((c, j) => (
                  <td key={j}>{c}</td>
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
}: {
  kind: ArtifactKind;
  title: string;
  data: Loaded;
  contentRef: React.RefObject<HTMLDivElement | null>;
}) {
  const t = useTranslate();
  switch (kind) {
    case "md":
      return (
        <div ref={contentRef} className="canvas-doc issue-reader text-secondary text-ink-200 break-words min-w-0" data-artifact-view="md">
          <Md text={decode(data)} />
        </div>
      );
    case "csv":
      return <CsvView text={decode(data)} contentRef={contentRef} />;
    case "html":
      return (
        <iframe
          className="canvas-frame"
          data-artifact-view="html"
          title={title}
          sandbox=""
          referrerPolicy="no-referrer"
          srcDoc={htmlFrameDocument(decode(data))}
        />
      );
    case "image":
      return (
        <div className="canvas-doc" data-artifact-view="image">
          <img className="canvas-image" src={imageDataUrl(data)} alt={title} />
        </div>
      );
    case "pdf":
      // No inline PDF: the board CSP allows no blob/object source, so the
      // drawer offers Download (in the footer) and says so.
      return (
        <div className="canvas-doc canvas-pdf" data-artifact-view="pdf">
          <p className="text-body text-ink-100">{title}</p>
          <p className="num text-micro text-ink-500">PDF · {sizeLabel(data.bytes.byteLength)}</p>
          <p className="text-micro text-ink-400">{t("A preview is not available yet. Use Download.")}</p>
        </div>
      );
  }
}
