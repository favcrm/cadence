import { useEffect, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Md from "../../ui/Md";
import { artifactBytes } from "./artifactClient";
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

function useObjectUrl(data: Loaded | null): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!data) return;
    const next = URL.createObjectURL(new Blob([data.bytes], { type: data.mime.split(";")[0] }));
    setUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [data]);
  return data ? url : null;
}

const decode = (data: Loaded): string => new TextDecoder("utf-8").decode(data.bytes);

/** Render one artifact version. Markdown through `Md`, CSV as text, HTML in
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
  const url = useObjectUrl(kind === "pdf" || kind === "image" ? data : null);
  switch (kind) {
    case "md":
      return (
        <div ref={contentRef} className="canvas-doc issue-reader text-secondary text-ink-200 break-words min-w-0" data-artifact-view="md">
          <Md text={decode(data)} />
        </div>
      );
    case "csv":
      return (
        <div ref={contentRef} className="canvas-doc min-w-0" data-artifact-view="csv">
          <pre className="num text-micro text-ink-200 whitespace-pre overflow-auto">{decode(data)}</pre>
        </div>
      );
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
          {url ? <img className="canvas-image" src={url} alt={title} /> : null}
        </div>
      );
    case "pdf":
      return (
        <div className="canvas-doc canvas-pdf" data-artifact-view="pdf">
          {url ? <object className="canvas-frame" data={url} type="application/pdf" aria-label={title} /> : null}
          <p className="text-micro text-ink-500">{t("If the PDF does not show here, use Download.")}</p>
        </div>
      );
  }
}
