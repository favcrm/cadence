// Host-mediated file download handler for sandboxed app frames (`sandbox="allow-scripts"`).
// Frames cannot initiate downloads directly without `allow-downloads`; this enforces strict
// source binding, canonical extension-to-MIME allowlists, safe basenames, and a 16 MiB cap.

export const SCREEN_FILE_DOWNLOAD_TYPE = "cadence-file-download";
export const SCREEN_FILE_DOWNLOAD_ACK_TYPE = "cadence-file-download-ack";
export const SCREEN_PDF_EXPORT_TYPE = "cadence-pdf-export";
export const SCREEN_PDF_EXPORT_ACK_TYPE = "cadence-pdf-export-ack";
export const SCREEN_IMPORT_FILE_TYPE = "cadence-import-file";
export const SCREEN_IMPORT_FILE_ACK_TYPE = "cadence-import-file-ack";
export const SCREEN_SUBMIT_DOCUMENT_TYPE = "cadence-submit-document";
export const SCREEN_SUBMIT_DOCUMENT_ACK_TYPE = "cadence-submit-document-ack";
export const SCREEN_FILE_DOWNLOAD_MAX_BYTES = 16 * 1024 * 1024;
export const SCREEN_PDF_EXPORT_HTML_MAX_BYTES = 2 * 1024 * 1024;
export const SCREEN_SUBMIT_DOCUMENT_MAX_BYTES = 10 * 1024 * 1024;
const SCREEN_FILE_NAME_MAX_CHARS = 120;
const REQUEST_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;

export interface StagedWorksImportFile {
  id: string;
  installId: string;
  fileName: string;
  mimeType: string;
  bytes: ArrayBuffer;
  createdAt: number;
}

const WORKS_IMPORT_TTL_MS = 60_000;
let importSeq = 0;
const stagedWorksImports = new Map<string, StagedWorksImportFile>();
const importListeners = new Set<(installId: string) => void>();

export function stageWorksImportFile(input: {
  installId: string;
  fileName: string;
  mimeType: string;
  bytes: ArrayBuffer;
}): string {
  const copy = new Uint8Array(input.bytes.byteLength);
  copy.set(new Uint8Array(input.bytes));
  const id = `imp_${Date.now().toString(36)}_${(++importSeq).toString(36)}`;
  stagedWorksImports.set(input.installId, {
    id,
    installId: input.installId,
    fileName: input.fileName,
    mimeType: input.mimeType,
    bytes: copy.buffer,
    createdAt: Date.now(),
  });
  for (const listener of importListeners) listener(input.installId);
  return id;
}

export function consumeWorksImportFile(installId: string): StagedWorksImportFile | null {
  const item = stagedWorksImports.get(installId) ?? null;
  if (!item) return null;
  stagedWorksImports.delete(installId);
  if (Date.now() - item.createdAt > WORKS_IMPORT_TTL_MS) return null;
  return item;
}

export function subscribeWorksImportFile(listener: (installId: string) => void): () => void {
  importListeners.add(listener);
  return () => importListeners.delete(listener);
}

const ALLOWED_DOWNLOAD_MIMES: ReadonlyMap<string, string> = new Map([
  ["md", "text/markdown;charset=utf-8"],
  ["html", "text/html;charset=utf-8"],
  ["csv", "text/csv;charset=utf-8"],
  ["pdf", "application/pdf"],
  ["xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"],
  ["pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"],
  ["docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
]);

export interface ValidatedScreenFileDownload {
  requestId: string;
  fileName: string;
  mimeType: string;
  bytes: ArrayBuffer;
}

function normalizeMimeBase(raw: string): string {
  return raw.split(";")[0]?.trim().toLowerCase() ?? "";
}

function isSafeDownloadFileName(raw: string): boolean {
  if (!raw || raw.length > SCREEN_FILE_NAME_MAX_CHARS) return false;
  if (raw.includes("/") || raw.includes("\\") || raw.includes("..") || raw === ".") return false;
  for (let i = 0; i < raw.length; i++) {
    const code = raw.charCodeAt(i);
    if (code < 32 || code === 127) return false;
  }
  return true;
}

function extractArrayBuffer(raw: unknown): ArrayBuffer | null {
  const view =
    raw instanceof ArrayBuffer
      ? new Uint8Array(raw)
      : raw instanceof Uint8Array
        ? raw
        : null;
  if (!view || view.byteLength === 0 || view.byteLength > SCREEN_FILE_DOWNLOAD_MAX_BYTES) {
    return null;
  }
  const copy = new Uint8Array(view.byteLength);
  copy.set(view);
  return copy.buffer;
}

export function parseScreenFileDownload(data: unknown): ValidatedScreenFileDownload | null {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const record = data as Record<string, unknown>;
  if (record.type !== SCREEN_FILE_DOWNLOAD_TYPE) return null;
  if (typeof record.requestId !== "string" || !REQUEST_ID_RE.test(record.requestId)) return null;
  if (typeof record.filename !== "string" || !isSafeDownloadFileName(record.filename)) return null;
  const dot = record.filename.lastIndexOf(".");
  if (dot <= 0 || dot === record.filename.length - 1) return null;
  const ext = record.filename.slice(dot + 1).toLowerCase();
  const canonicalMime = ALLOWED_DOWNLOAD_MIMES.get(ext);
  if (!canonicalMime) return null;
  if (typeof record.mimeType !== "string") return null;
  if (normalizeMimeBase(record.mimeType) !== normalizeMimeBase(canonicalMime)) return null;
  const bytes = extractArrayBuffer(record.bytes);
  if (!bytes) return null;
  return {
    requestId: record.requestId,
    fileName: record.filename,
    mimeType: canonicalMime,
    bytes,
  };
}

export interface DownloadMessageEventLike {
  source: MessageEventSource | null;
  data: unknown;
}

export interface DownloadReplyTarget {
  postMessage(message: unknown, targetOrigin: string): void;
}

export function handleScreenFileDownloadMessage(
  event: DownloadMessageEventLike,
  expectedSource: DownloadReplyTarget | null,
  isCurrent: () => boolean,
  saveFile: (bytes: ArrayBuffer, mimeType: string, fileName: string) => void,
): boolean {
  const data = event.data as Record<string, unknown> | null;
  if (!data || typeof data !== "object" || data.type !== SCREEN_FILE_DOWNLOAD_TYPE) {
    return false;
  }
  if (!expectedSource || event.source !== (expectedSource as unknown as MessageEventSource) || !isCurrent()) {
    return true;
  }
  const parsed = parseScreenFileDownload(data);
  if (!parsed) return true;
  saveFile(parsed.bytes, parsed.mimeType, parsed.fileName);
  expectedSource.postMessage(
    { type: SCREEN_FILE_DOWNLOAD_ACK_TYPE, requestId: parsed.requestId },
    "*",
  );
  return true;
}

export interface ValidatedScreenPdfExport {
  requestId: string;
  fileName: string;
  html: string;
  paper?: "a4" | "letter";
}

const utf8Encoder = new TextEncoder();

export function parseScreenPdfExport(data: unknown): ValidatedScreenPdfExport | null {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const record = data as Record<string, unknown>;
  if (record.type !== SCREEN_PDF_EXPORT_TYPE) return null;
  if (typeof record.requestId !== "string" || !REQUEST_ID_RE.test(record.requestId)) return null;
  if (typeof record.filename !== "string" || !isSafeDownloadFileName(record.filename)) return null;
  if (!record.filename.toLowerCase().endsWith(".pdf") || record.filename.length <= 4) return null;
  if (typeof record.html !== "string" || record.html.trim().length === 0) return null;
  if (utf8Encoder.encode(record.html).byteLength > SCREEN_PDF_EXPORT_HTML_MAX_BYTES) return null;
  const paper = record.paper;
  if (paper !== undefined && paper !== "a4" && paper !== "letter") return null;
  return {
    requestId: record.requestId,
    fileName: record.filename,
    html: record.html,
    ...(paper ? { paper } : {}),
  };
}

export function handleScreenPdfExportMessage(
  event: DownloadMessageEventLike,
  expectedSource: DownloadReplyTarget | null,
  isCurrent: () => boolean,
  exportPdf: (request: ValidatedScreenPdfExport) => Promise<void>,
): boolean {
  const data = event.data as Record<string, unknown> | null;
  if (!data || typeof data !== "object" || data.type !== SCREEN_PDF_EXPORT_TYPE) {
    return false;
  }
  if (!expectedSource || event.source !== (expectedSource as unknown as MessageEventSource) || !isCurrent()) {
    return true;
  }
  const parsed = parseScreenPdfExport(data);
  if (!parsed) {
    const rawId = typeof data.requestId === "string" && REQUEST_ID_RE.test(data.requestId) ? data.requestId : null;
    if (rawId) {
      expectedSource.postMessage(
        { type: SCREEN_PDF_EXPORT_ACK_TYPE, requestId: rawId, ok: false, error: "Invalid PDF export request." },
        "*",
      );
    }
    return true;
  }
  void exportPdf(parsed).then(
    () => {
      if (!isCurrent()) return;
      expectedSource.postMessage(
        { type: SCREEN_PDF_EXPORT_ACK_TYPE, requestId: parsed.requestId, ok: true },
        "*",
      );
    },
    (error: unknown) => {
      if (!isCurrent()) return;
      expectedSource.postMessage(
        {
          type: SCREEN_PDF_EXPORT_ACK_TYPE,
          requestId: parsed.requestId,
          ok: false,
          error: error instanceof Error ? error.message : "PDF export failed.",
        },
        "*",
      );
    },
  );
  return true;
}

export type SubmittedOfficeKind = "docx" | "xlsx" | "pptx";

export type ValidatedScreenSubmitDocument =
  | {
      requestId: string;
      title: string;
      label: string;
      kind: "pdf";
      html: string;
      paper?: "a4" | "letter";
      submit: boolean;
    }
  | {
      requestId: string;
      title: string;
      label: string;
      kind: SubmittedOfficeKind;
      bytes: ArrayBuffer;
      submit: boolean;
    };

function hasControlChars(raw: string): boolean {
  for (let i = 0; i < raw.length; i++) {
    const code = raw.charCodeAt(i);
    if (code < 32 || code === 127) return true;
  }
  return false;
}

function isZipMagic(bytes: ArrayBuffer): boolean {
  const head = new Uint8Array(bytes, 0, Math.min(4, bytes.byteLength));
  return head.length >= 4 && head[0] === 0x50 && head[1] === 0x4b && head[2] === 0x03 && head[3] === 0x04;
}

export function parseScreenSubmitDocument(data: unknown): ValidatedScreenSubmitDocument | null {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const record = data as Record<string, unknown>;
  if (record.type !== SCREEN_SUBMIT_DOCUMENT_TYPE) return null;
  if (typeof record.requestId !== "string" || !REQUEST_ID_RE.test(record.requestId)) return null;
  if (typeof record.title !== "string") return null;
  const title = record.title.trim();
  if (!title || title.length > 120 || hasControlChars(title)) return null;
  if (typeof record.label !== "string") return null;
  const label = record.label.trim();
  if (!label || label.length > 40 || hasControlChars(label) || label.includes("/") || label.includes("\\")) return null;
  if (typeof record.submit !== "boolean") return null;

  if (record.kind === "pdf") {
    if (typeof record.html !== "string" || record.html.trim().length === 0) return null;
    if (utf8Encoder.encode(record.html).byteLength > SCREEN_PDF_EXPORT_HTML_MAX_BYTES) return null;
    const paper = record.paper;
    if (paper !== undefined && paper !== "a4" && paper !== "letter") return null;
    return {
      requestId: record.requestId,
      title,
      label,
      kind: "pdf",
      html: record.html,
      ...(paper ? { paper } : {}),
      submit: record.submit,
    };
  }

  if (record.kind === "docx" || record.kind === "xlsx" || record.kind === "pptx") {
    const view =
      record.bytes instanceof ArrayBuffer
        ? new Uint8Array(record.bytes)
        : record.bytes instanceof Uint8Array
          ? record.bytes
          : null;
    if (!view || view.byteLength === 0 || view.byteLength > SCREEN_SUBMIT_DOCUMENT_MAX_BYTES) return null;
    const copy = new Uint8Array(view.byteLength);
    copy.set(view);
    if (!isZipMagic(copy.buffer)) return null;
    return {
      requestId: record.requestId,
      title,
      label,
      kind: record.kind,
      bytes: copy.buffer,
      submit: record.submit,
    };
  }

  return null;
}

export function handleScreenSubmitDocumentMessage(
  event: DownloadMessageEventLike,
  expectedSource: DownloadReplyTarget | null,
  isCurrent: () => boolean,
  submitDocument: (request: ValidatedScreenSubmitDocument) => Promise<{ id: string; stage: string }>,
): boolean {
  const data = event.data as Record<string, unknown> | null;
  if (!data || typeof data !== "object" || data.type !== SCREEN_SUBMIT_DOCUMENT_TYPE) {
    return false;
  }
  if (!expectedSource || event.source !== (expectedSource as unknown as MessageEventSource) || !isCurrent()) {
    return true;
  }
  const parsed = parseScreenSubmitDocument(data);
  if (!parsed) {
    const rawId = typeof data.requestId === "string" && REQUEST_ID_RE.test(data.requestId) ? data.requestId : null;
    if (rawId) {
      expectedSource.postMessage(
        { type: SCREEN_SUBMIT_DOCUMENT_ACK_TYPE, requestId: rawId, ok: false, error: "Invalid document submission request." },
        "*",
      );
    }
    return true;
  }
  void submitDocument(parsed).then(
    (res) => {
      if (!isCurrent()) return;
      expectedSource.postMessage(
        { type: SCREEN_SUBMIT_DOCUMENT_ACK_TYPE, requestId: parsed.requestId, ok: true, id: res.id, stage: res.stage },
        "*",
      );
    },
    (error: unknown) => {
      if (!isCurrent()) return;
      expectedSource.postMessage(
        {
          type: SCREEN_SUBMIT_DOCUMENT_ACK_TYPE,
          requestId: parsed.requestId,
          ok: false,
          error: error instanceof Error ? error.message : "Document submission failed.",
        },
        "*",
      );
    },
  );
  return true;
}
