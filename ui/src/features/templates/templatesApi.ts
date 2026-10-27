import { errorMessage } from "../../lib/apiError";
import { sessionHeaders } from "../../lib/sessionKey";
import { privateFetch, usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";

/**
 * The company document-template HTTP surface (CAD-1393). Each call relays
 * one daemon method; the daemon owns scope, the approval digest and every
 * refusal, so this module only shapes requests and surfaces errors.
 *
 *   POST /api/templates/create   {name, docx_base64, spec}
 *   POST /api/templates/revise   {name, base, docx_base64, spec}
 *   POST /api/templates/approve  {name, version, digest, source}
 *   GET  /api/templates/show?name=&version=
 *   POST /api/documents/create   {template, version, data}
 *   GET  /api/documents/show?id=
 *   GET  /api/documents/export?id=   (DOCX and PDF as base64)
 *   GET  /api/templates/sample?name=&version=   (labelled sample, draft or approved)
 *   POST /api/templates/intake   {file, source}  (read-only reference extraction)
 */

export interface TemplateSummary {
  name: string;
  version: number;
  based_on: number | null;
  status: "draft" | "approved";
  kind: string;
  content_sha256: string;
  source_sha256: string;
  approved_source: string | null;
  created: number;
}

export interface TemplateSpec {
  kind: "quotation" | "invoice";
  fields: { key: string; label: string; kind: string; required: boolean }[];
  fixed_text: Record<string, string>;
  calculation: { confirmed: boolean; currency: string; tax_rate_bps: number; discount_bps: number };
}

export interface TemplateView {
  template: TemplateSummary;
  spec: TemplateSpec;
}

export interface TemplateList {
  name: string;
  versions: TemplateSummary[];
}

export interface DraftView {
  id: string;
  template: string;
  version: number;
  template_sha256: string;
  kind: string;
  state: "draft";
  input: unknown;
  context: Record<string, unknown>;
  created: number;
}

/** One extracted reference block: a paragraph or table cell with its locator. */
export interface IntakeBlock {
  locator: string;
  style?: string;
  text: string;
}

/** Structure the extractor reports for a DOCX reference. It proposes nothing. */
export interface IntakeStructure {
  text_blocks: number;
  placeholders: string[];
  table_placeholders: string[];
  loops: { locator: string; tag: string }[];
  unrecognized_markers: { locator: string; text: string }[];
  tables: number;
  images: number;
  media_parts: string[];
  styles: string[];
}

/** The provenance record the sandbox writes for one reference. */
export interface IntakeProvenance {
  source: { filename: string; sha256: string; preserved_as: string };
  evidence:
    | { format: "docx"; blocks: IntakeBlock[]; structure: IntakeStructure }
    | { format: "pdf"; pages: { page: number; text: string }[] };
  reconstruction?: { file: string; kind: string; limitations: string[] };
}

export interface IntakeView {
  file: string;
  provenance: IntakeProvenance;
  reconstruction_base64?: string;
}

export interface SampleView {
  sample: true;
  status: "draft" | "approved";
  template: string;
  version: number;
  context: Record<string, unknown>;
  docx_base64: string;
  pdf_base64: string;
}

export interface ExportView {
  id: string;
  docx_base64: string;
  pdf_base64: string;
}

/** Decode base64 export bytes into a downloadable blob. */
export function base64Blob(base64: string, type: string): Blob {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new Blob([bytes], { type });
}

/** A refusal the daemon or board gave, with its message. */
export class TemplateError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

async function read<T>(url: string, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, { headers: sessionHeaders() }, op);
    const value = await answer<T>(resp);
    op.assertCurrent();
    return value;
  });
}

async function write<T>(url: string, body: unknown, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify(body),
    }, op);
    const value = await answer<T>(resp);
    op.assertCurrent();
    return value;
  });
}

async function answer<T>(resp: Response): Promise<T> {
  const body = await resp.json().catch(() => null);
  if (!resp.ok) {
    const message = errorMessage(body, `${resp.status} ${resp.statusText}`);
    throw new TemplateError(message, resp.status);
  }
  return body as T;
}

/** The DOCX bytes as base64 for the JSON body. */
export function docxBase64(bytes: ArrayBuffer): string {
  let binary = "";
  const view = new Uint8Array(bytes);
  for (let i = 0; i < view.length; i += 0x8000) {
    binary += String.fromCharCode(...view.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

export const templatesApi = {
  create: (name: string, docx_base64: string, spec: unknown) =>
    write<TemplateView>("/api/templates/create", { name, docx_base64, spec }),
  revise: (name: string, base: number, docx_base64: string, spec: unknown) =>
    write<TemplateView>("/api/templates/revise", { name, base, docx_base64, spec }),
  approve: (name: string, version: number, digest: string, source: string) =>
    write<TemplateView>("/api/templates/approve", { name, version, digest, source }),
  list: (name: string) => read<TemplateList>(`/api/templates/show?name=${encodeURIComponent(name)}`),
  version: (name: string, version: number) =>
    read<TemplateView>(`/api/templates/show?name=${encodeURIComponent(name)}&version=${version}`),
  createDocument: (template: string, version: number, data: unknown) =>
    write<DraftView>("/api/documents/create", { template, version, data }),
  document: (id: string) => read<DraftView>(`/api/documents/show?id=${encodeURIComponent(id)}`),
  exportDocument: (id: string) => read<ExportView>(`/api/documents/export?id=${encodeURIComponent(id)}`),
  intake: (file: string, source: string) => write<IntakeView>("/api/templates/intake", { file, source }),
  sample: (name: string, version: number) =>
    read<SampleView>(`/api/templates/sample?name=${encodeURIComponent(name)}&version=${version}`),
};
