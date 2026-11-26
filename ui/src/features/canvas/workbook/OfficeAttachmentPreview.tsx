import { useEffect, useState } from "react";
import { useLocaleTag } from "../../../lib/locale";
import { beginPrivateOperation, type PrivateOperation } from "../../../lib/privateSession";
import DocxContent from "./DocxContent";
import SlidesContent from "./SlidesContent";
import WorkbookPreview from "./WorkbookPreview";
import { OFFICE_FORMATS, type OfficeFormat, type OfficePreviewRequest, type OfficePreviewResult } from "./officePreviewTypes";
import "./workbook.css";

const MAX_BYTES = 25 * 1024 * 1024;

type State = { status: "loading" | "error" } | { status: "ready"; result: OfficePreviewResult };

async function readBytes(href: string, format: OfficeFormat, operation: PrivateOperation): Promise<ArrayBuffer> {
  const url = new URL(href, window.location.href);
  if (url.origin !== window.location.origin) throw new Error("Cross-origin attachment");
  const response = await operation.fetch(url, { credentials: "same-origin", redirect: "error" });
  const mime = response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (!response.ok || mime === undefined || OFFICE_FORMATS[mime] !== format || !response.body) {
    throw new Error("Unavailable attachment");
  }
  const length = response.headers.get("content-length");
  if (length && Number(length) > MAX_BYTES) throw new Error("Attachment too large");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_BYTES) {
        await reader.cancel();
        throw new Error("Attachment too large");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return bytes.buffer;
}

export default function OfficeAttachmentPreview({ href, title, format }: { href: string; title: string; format: OfficeFormat }) {
  const zh = useLocaleTag() === "zh-TW";
  const [state, setState] = useState<State>({ status: "loading" });

  useEffect(() => {
    let operation: PrivateOperation | undefined;
    let worker: Worker | undefined;
    let active = true;
    setState({ status: "loading" });
    void (async () => {
      try {
        const pending = beginPrivateOperation();
        operation = pending;
        pending.signal.addEventListener("abort", () => worker?.terminate(), { once: true });
        const bytes = await readBytes(href, format, pending);
        // Vite supplies the worker URL only in the browser bundle.
        const { default: workerUrl } = await import("./office.worker?worker&url");
        pending.assertCurrent();
        worker = new Worker(workerUrl, { type: "module" });
        worker.onmessage = (event: MessageEvent<OfficePreviewResult>) => {
          if (active && !pending.signal.aborted) {
            setState(event.data.format === format ? { status: "ready", result: event.data } : { status: "error" });
          }
          pending.dispose();
        };
        worker.onerror = () => {
          if (active && !pending.signal.aborted) setState({ status: "error" });
          pending.dispose();
        };
        const request: OfficePreviewRequest = { format, bytes };
        worker.postMessage(request, [bytes]);
      } catch {
        if (active && !operation?.signal.aborted) setState({ status: "error" });
        operation?.dispose();
      }
    })();
    return () => { active = false; operation?.dispose(); };
  }, [href, format]);

  if (state.status === "loading") {
    return <div className="workbook-status" role="status">{format === "xlsx"
      ? zh ? "正在載入試算表…" : "Loading workbook…"
      : zh ? "正在載入附件預覽…" : "Loading attachment preview…"}</div>;
  }
  if (state.status === "ready" && state.result.format === format) {
    switch (state.result.format) {
      case "docx": return <DocxContent doc={state.result.doc} title={title} />;
      case "pptx": return <SlidesContent deck={state.result.deck} title={title} />;
      case "xlsx": return <WorkbookPreview workbook={state.result.workbook} title={title} />;
    }
  }
  return <div className="workbook-status" role="alert">{format === "xlsx"
    ? zh ? "無法預覽此試算表。檔案可能無法使用、格式不支援或超過大小限制。" : "This workbook cannot be previewed. It may be unavailable, unsupported, or too large."
    : zh ? "無法預覽此附件。檔案可能無法使用、格式不支援或超過大小限制。" : "This attachment cannot be previewed. It may be unavailable, unsupported, or too large."}</div>;
}
