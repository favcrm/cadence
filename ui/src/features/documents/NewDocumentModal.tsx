import { useEffect, useId, useRef, useState } from "react";
import type React from "react";
import { useTranslate } from "../../lib/locale";
import {
  DOCUMENT_MAX_BYTES,
  create,
  refusalCode,
  type CreateDocumentInput,
  type DocumentKind,
  type DocumentRow,
  type DocumentType,
} from "./documentsClient";

type Translate = (english: string) => string;
type CreateMode = "write" | "upload";

const hasControl = (value: string) =>
  Array.from(value).some((ch) => {
    const code = ch.charCodeAt(0);
    return code < 0x20 || code === 0x7f;
  });

const pathUnsafeLabel = (label: string) => /[/\\]/.test(label) || /^\.+$/.test(label);

/** Mirror server `validDocumentLabel`: trimmed 1..40 chars, path-safe, no control chars. */
export function validDocumentLabelInput(raw: string): string | null {
  if (hasControl(raw)) return null;
  const label = raw.trim();
  if (label.length < 1 || label.length > 40 || pathUnsafeLabel(label)) return null;
  return label;
}

const EXTENSION_KIND: Record<string, DocumentKind> = {
  ".md": "md",
  ".markdown": "md",
  ".html": "html",
  ".htm": "html",
  ".csv": "csv",
  ".pdf": "pdf",
  ".png": "image",
  ".jpg": "image",
  ".jpeg": "image",
  ".webp": "image",
  ".gif": "image",
  ".docx": "docx",
  ".xlsx": "xlsx",
  ".pptx": "pptx",
};

const MIME_KIND: Record<string, DocumentKind> = {
  "text/markdown": "md",
  "text/x-markdown": "md",
  "text/html": "html",
  "text/csv": "csv",
  "application/pdf": "pdf",
  "image/png": "image",
  "image/jpeg": "image",
  "image/webp": "image",
  "image/gif": "image",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
};

/** Map a local file name and browser MIME type onto a board `ArtifactKind`, or `null` when unsupported. */
export function inferDocumentKind(fileName: string, mimeType: string): DocumentKind | null {
  const normalizedName = fileName.trim().toLowerCase();
  const dot = normalizedName.lastIndexOf(".");
  if (dot >= 0) {
    const byExt = EXTENSION_KIND[normalizedName.slice(dot)];
    if (byExt) return byExt;
  }
  const cleanMime = mimeType.split(";")[0]?.trim().toLowerCase() ?? "";
  return MIME_KIND[cleanMime] ?? null;
}

/** Encode arbitrary bytes into standard Base64 in fixed chunks so large uploads never overflow arguments. */
export function bytesToBase64(bytes: Uint8Array): string {
  const chunk = 0x4000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** Encode a UTF-8 string (including CJK / emoji) cleanly into Base64. */
export function utf8ToBase64(text: string): string {
  return bytesToBase64(new TextEncoder().encode(text));
}

export function createDocumentFailure(code: string | null, message: string | null, t: Translate): string {
  if (code === "payload_too_large") return t("Documents are at most 10 MiB.");
  if (code === "not_found") return t("This project or linked record could not be found.");
  if (code === "owner_required") return t("Only the workspace’s own members can create documents.");
  if (code === "membership_changed") return t("Your access changed. Reload the page and try again.");
  if (code === "invalid_request") return message ?? t("Check the document details and try again.");
  return t("Couldn’t create this document. Try again.");
}

function titleFromFileName(fileName: string): string {
  const trimmed = fileName.trim();
  const dot = trimmed.lastIndexOf(".");
  const base = dot > 0 ? trimmed.slice(0, dot) : trimmed;
  return base.slice(0, 120);
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;
}

const KIND_BADGE: Record<DocumentKind, string> = {
  md: "MD",
  html: "HTML",
  csv: "CSV",
  pdf: "PDF",
  image: "IMAGE",
  docx: "DOCX",
  xlsx: "XLSX",
  pptx: "PPTX",
};

interface SelectedFile {
  name: string;
  size: number;
  kind: DocumentKind;
  bytes: Uint8Array;
}

export default function NewDocumentModal({
  open,
  project,
  types,
  returnFocusTo,
  onClose,
  onCreated,
}: {
  open: boolean;
  project: string | null;
  types: readonly DocumentType[];
  returnFocusTo?: HTMLElement | null;
  onClose: () => void;
  onCreated: (row: DocumentRow, submitted: boolean) => void;
}) {
  const t = useTranslate();
  const titleId = useId();
  const nameInputId = useId();
  const typeInputId = useId();
  const contentInputId = useId();
  const fileInputId = useId();

  const dialogRef = useRef<HTMLElement | null>(null);
  const titleInputRef = useRef<HTMLInputElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const requestSeqRef = useRef(0);

  const [mode, setMode] = useState<CreateMode>("write");
  const [title, setTitle] = useState("");
  const [label, setLabel] = useState("");
  const [markdown, setMarkdown] = useState("");
  const [selectedFile, setSelectedFile] = useState<SelectedFile | null>(null);
  const [readingFile, setReadingFile] = useState(false);
  const [busyAction, setBusyAction] = useState<"draft" | "submit" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const busy = busyAction !== null;

  useEffect(() => {
    if (!open) return;
    const active = document.activeElement;
    openerRef.current = active instanceof HTMLElement ? active : null;
    setMode("write");
    setTitle("");
    setLabel("");
    setMarkdown("");
    setSelectedFile(null);
    setReadingFile(false);
    setBusyAction(null);
    setError(null);
    requestAnimationFrame(() => {
      titleInputRef.current?.focus();
    });
    return () => {
      const target = returnFocusTo ?? openerRef.current;
      if (target?.isConnected) target.focus();
    };
  }, [open, returnFocusTo]);

  if (!open) return null;

  const suggestedLabels: string[] = [];
  const seenLabels = new Set<string>();
  for (const row of types) {
    const clean = row.label.trim();
    const key = clean.toLowerCase();
    if (!clean || seenLabels.has(key)) continue;
    seenLabels.add(key);
    suggestedLabels.push(clean);
  }

  const focusable = (): HTMLElement[] => {
    const root = dialogRef.current;
    if (!root) return [];
    return Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]):not([type="file"]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.getAttribute("aria-hidden") !== "true");
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (!busy) onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      dialogRef.current?.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || active === dialogRef.current) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || active === dialogRef.current) {
      event.preventDefault();
      first.focus();
    }
  };

  const handleFile = async (file: File | null) => {
    setError(null);
    if (!file) return;
    const kind = inferDocumentKind(file.name, file.type);
    if (!kind) {
      setSelectedFile(null);
      setError(t("Unsupported file type. Choose .md, .html, .csv, .pdf, or an image."));
      return;
    }
    if (file.size <= 0) {
      setSelectedFile(null);
      setError(t("The selected file is empty."));
      return;
    }
    if (file.size > DOCUMENT_MAX_BYTES) {
      setSelectedFile(null);
      setError(t("Documents are at most 10 MiB."));
      return;
    }
    setReadingFile(true);
    try {
      const buffer = await file.arrayBuffer();
      const bytes = new Uint8Array(buffer);
      if (bytes.length === 0) {
        setSelectedFile(null);
        setError(t("The selected file is empty."));
        return;
      }
      if (bytes.length > DOCUMENT_MAX_BYTES) {
        setSelectedFile(null);
        setError(t("Documents are at most 10 MiB."));
        return;
      }
      setSelectedFile({ name: file.name, size: bytes.length, kind, bytes });
      if (!title.trim()) {
        setTitle(titleFromFileName(file.name));
      }
    } catch {
      setSelectedFile(null);
      setError(t("Couldn’t read the selected file. Try again."));
    } finally {
      setReadingFile(false);
    }
  };

  const submitForm = async (submitNow: boolean) => {
    if (busy || readingFile) return;
    if (hasControl(title)) {
      setError(t("Document title cannot contain control characters."));
      titleInputRef.current?.focus();
      return;
    }
    const cleanTitle = title.trim();
    if (!cleanTitle || cleanTitle.length > 120) {
      setError(t("Document title is required."));
      titleInputRef.current?.focus();
      return;
    }
    const cleanLabel = validDocumentLabelInput(label);
    if (!cleanLabel) {
      setError(t("Enter a valid document type label (1–40 characters, no slashes)."));
      return;
    }

    let kind: DocumentKind;
    let contentBase64: string;
    if (mode === "write") {
      if (markdown.trim().length === 0) {
        setError(t("Write some Markdown content before saving."));
        return;
      }
      const encodedBytes = new TextEncoder().encode(markdown);
      if (encodedBytes.length > DOCUMENT_MAX_BYTES) {
        setError(t("Documents are at most 10 MiB."));
        return;
      }
      kind = "md";
      contentBase64 = bytesToBase64(encodedBytes);
    } else {
      if (!selectedFile) {
        setError(t("Choose a file (.md, .html, .csv, .pdf, or image) to upload."));
        return;
      }
      kind = selectedFile.kind;
      contentBase64 = bytesToBase64(selectedFile.bytes);
    }

    const requestKey = `board-ui-${Date.now()}-${++requestSeqRef.current}`;
    const payload: CreateDocumentInput = {
      title: cleanTitle,
      label: cleanLabel,
      kind,
      contentBase64,
      ...(project ? { project } : {}),
      submit: submitNow,
      requestKey,
    };

    setBusyAction(submitNow ? "submit" : "draft");
    setError(null);
    try {
      const result = await create(payload);
      if (result.ok) {
        onCreated(result.value, submitNow);
        return;
      }
      setError(createDocumentFailure(refusalCode(result), result.failure.message, t));
    } catch {
      onClose();
    } finally {
      setBusyAction(null);
    }
  };

  return (
    <div className="s-confirm-wrap" role="presentation">
      <div
        className="s-confirm-scrim"
        onClick={() => {
          if (!busy) onClose();
        }}
      />
      <section
        ref={dialogRef}
        tabIndex={-1}
        className="documents-create-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={handleKeyDown}
      >
        <header className="documents-create-head">
          <div>
            <div className="slabel">
              {project
                ? t("Project · {project}").replace("{project}", () => project)
                : t("Workspace documents")}
            </div>
            <h2 id={titleId} className="documents-create-title">
              {t("New document")}
            </h2>
          </div>
          <button
            type="button"
            className="btn btn-sm btn-ghost"
            onClick={() => {
              if (!busy) onClose();
            }}
            disabled={busy}
            aria-label={t("Close")}
          >
            ✕
          </button>
        </header>

        <form
          className="documents-create-form"
          onSubmit={(event) => {
            event.preventDefault();
            void submitForm(false);
          }}
          noValidate
        >
          <div className="documents-create-mode" role="group" aria-label={t("Document source")}>
            {(
              [
                { value: "write", label: "Write Markdown" },
                { value: "upload", label: "Upload file" },
              ] as const
            ).map((opt) => {
              const selected = mode === opt.value;
              return (
                <button
                  key={opt.value}
                  type="button"
                  className={`documents-create-mode-opt${selected ? " is-selected" : ""}`}
                  aria-pressed={selected}
                  onClick={() => {
                    setMode(opt.value);
                    setError(null);
                  }}
                  disabled={busy}
                >
                  {t(opt.label)}
                </button>
              );
            })}
          </div>

          <div className="documents-create-field">
            <label htmlFor={nameInputId} className="documents-create-label">
              {t("Title")}
            </label>
            <input
              id={nameInputId}
              ref={titleInputRef}
              type="text"
              className="documents-create-input"
              value={title}
              onChange={(event) => {
                setTitle(event.target.value);
                setError(null);
              }}
              placeholder={t("e.g. Quarterly proposal")}
              maxLength={120}
              autoComplete="off"
              required
              disabled={busy}
            />
          </div>

          <div className="documents-create-field">
            <label htmlFor={typeInputId} className="documents-create-label">
              {t("Document type")}
            </label>
            <input
              id={typeInputId}
              type="text"
              className="documents-create-input"
              value={label}
              onChange={(event) => {
                setLabel(event.target.value);
                setError(null);
              }}
              placeholder={t("e.g. brief, proposal, report")}
              maxLength={40}
              autoComplete="off"
              required
              disabled={busy}
            />
            {suggestedLabels.length > 0 && (
              <div className="documents-create-type-chips" role="group" aria-label={t("Existing document types")}>
                {suggestedLabels.map((item) => {
                  const selected = label.trim().toLowerCase() === item.toLowerCase();
                  return (
                    <button
                      key={item}
                      type="button"
                      className={`documents-create-type-chip${selected ? " is-selected" : ""}`}
                      aria-pressed={selected}
                      onClick={() => {
                        setLabel(item);
                        setError(null);
                      }}
                      disabled={busy}
                    >
                      {item}
                    </button>
                  );
                })}
              </div>
            )}
          </div>

          {mode === "write" ? (
            <div className="documents-create-field">
              <label htmlFor={contentInputId} className="documents-create-label">
                {t("Markdown content")}
              </label>
              <textarea
                id={contentInputId}
                className="documents-create-textarea"
                rows={7}
                value={markdown}
                onChange={(event) => {
                  setMarkdown(event.target.value);
                  setError(null);
                }}
                placeholder={t("# Title\n\nWrite your deliverable in Markdown…")}
                disabled={busy}
              />
            </div>
          ) : (
            <div className="documents-create-field">
              <label htmlFor={fileInputId} className="documents-create-label">
                {t("Deliverable file")}
              </label>
              <input
                id={fileInputId}
                ref={fileInputRef}
                type="file"
                className="sr-only"
                accept=".md,.markdown,.html,.htm,.csv,.pdf,.png,.jpg,.jpeg,.webp,.gif,.docx,.xlsx,.pptx,text/markdown,text/html,text/csv,application/pdf,image/png,image/jpeg,image/webp,image/gif,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet,application/vnd.openxmlformats-officedocument.presentationml.presentation"
                onChange={(event) => {
                  const picked = event.target.files?.[0] ?? null;
                  void handleFile(picked);
                }}
                disabled={busy}
              />
              <div
                className="documents-create-dropzone"
                onDragOver={(event) => {
                  event.preventDefault();
                }}
                onDrop={(event) => {
                  event.preventDefault();
                  if (!busy) void handleFile(event.dataTransfer.files?.[0] ?? null);
                }}
              >
                {selectedFile ? (
                  <div className="documents-create-file-meta">
                    <span className="documents-create-file-name">{selectedFile.name}</span>
                    <span className="documents-create-file-badges">
                      <span className="chip documents-version-chip">{KIND_BADGE[selectedFile.kind]}</span>
                      <span className="num text-label text-ink-400">{formatFileSize(selectedFile.size)}</span>
                    </span>
                  </div>
                ) : (
                  <p className="documents-create-dropzone-hint">
                    {t("Drop a file here or browse (.md, .html, .csv, .pdf, images up to 10 MiB).")}
                  </p>
                )}
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => fileInputRef.current?.click()}
                  disabled={busy || readingFile}
                >
                  {selectedFile ? t("Replace file") : t("Choose file")}
                </button>
              </div>
            </div>
          )}

          {error && (
            <div className="documents-create-error" role="alert">
              {error}
            </div>
          )}

          <footer className="documents-create-foot">
            <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy}>
              {t("Cancel")}
            </button>
            <button type="submit" className="btn" disabled={busy || readingFile}>
              {busyAction === "draft" ? t("Saving…") : t("Save as draft")}
            </button>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => void submitForm(true)}
              disabled={busy || readingFile}
            >
              {busyAction === "submit" ? t("Submitting…") : t("Submit for review")}
            </button>
          </footer>
        </form>
      </section>
    </div>
  );
}
