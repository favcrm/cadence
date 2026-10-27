import { createSha256 } from "../app-shell/csvDigest";
import { sessionHeaders } from "../../lib/sessionKey";
import { notifyPrivateSessionRequired, privateFetch, type PrivateOperation } from "../../lib/privateSession";
import { wikiErrorFrom, WikiError, wiki } from "./api";

export const CHUNK_BYTES = 16384;
const MAX_FILE_BYTES = 100 * 1024 * 1024;
const MAX_TEXT_BYTES = 64 * 1024 * 1024;
const RESUME_KEY = "cadence.wiki.transfer.resume";

export interface ResumeEntry {
  job_id: string;
  digest: string;
  root: string;
  update: boolean;
  id: string;
  path: string;
  kind: "text" | "blob";
  size: number;
  sha256: string;
  if_rev: string;
}

export interface ImportResult {
  outcome: string;
  extraction: string;
  index: string;
  rev?: string;
  error?: string;
}

function record(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function whole(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

export function entryKind(name: string): "text" | "blob" {
  const lower = name.toLowerCase();
  return lower.endsWith(".md") || lower.endsWith(".markdown") || lower.endsWith(".txt") ? "text" : "blob";
}

export function entryId(name: string, index: number): string {
  const cleaned = name.replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 70);
  const id = cleaned.length > 0 ? `${cleaned}_${index}` : `file_${index}`;
  return id.slice(0, 80);
}

export async function sha256File(file: Blob): Promise<string> {
  const hash = createSha256();
  let offset = 0;
  while (offset < file.size) {
    const end = Math.min(file.size, offset + CHUNK_BYTES);
    hash.update(new Uint8Array(await file.slice(offset, end).arrayBuffer()));
    offset = end;
  }
  return hash.hex();
}

function bytesBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += step) {
    const slice = bytes.subarray(offset, offset + step);
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

async function transferCall(path: string, transfer: object, writing: boolean, operation: PrivateOperation): Promise<Record<string, unknown>> {
  const resp = await privateFetch(writing ? "/api/wiki/transfer" : "/api/wiki/job_status", {
    method: writing ? "PUT" : "POST",
    headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
    body: JSON.stringify({ path, transfer }),
  }, operation);
  const body: unknown = await resp.json().catch(() => null);
  operation.assertCurrent();
  if (resp.status === 401) notifyPrivateSessionRequired(resp.status, body, operation);
  if (!resp.ok) throw wikiErrorFrom(resp.status, body);
  const row = record(body);
  if (!row) throw new WikiError("wiki transfer answered without a record", resp.status);
  return row;
}

export function readResume(root: string): ResumeEntry[] {
  try {
    const parsed: unknown = JSON.parse(sessionStorage.getItem(RESUME_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((item): item is ResumeEntry => {
      const row = record(item);
      return !!row && row.root === root && typeof row.job_id === "string" && typeof row.sha256 === "string" && typeof row.path === "string";
    });
  } catch {
    return [];
  }
}

export function writeResume(entry: ResumeEntry): void {
  const all = readResumeAll().filter((item) => !(item.root === entry.root && item.sha256 === entry.sha256 && item.path === entry.path));
  all.push(entry);
  sessionStorage.setItem(RESUME_KEY, JSON.stringify(all));
}

export function forgetResume(root: string, sha256?: string): void {
  const all = readResumeAll().filter((item) => item.root !== root || (sha256 !== undefined && item.sha256 !== sha256));
  sessionStorage.setItem(RESUME_KEY, JSON.stringify(all));
}

export function clearTransferResume(): void {
  sessionStorage.removeItem(RESUME_KEY);
}

function readResumeAll(): ResumeEntry[] {
  try {
    const parsed: unknown = JSON.parse(sessionStorage.getItem(RESUME_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((item): item is ResumeEntry => record(item)?.job_id !== undefined && typeof record(item)?.root === "string");
  } catch {
    return [];
  }
}

async function destinationRev(root: string, name: string, update: boolean): Promise<string> {
  if (!update) return "none";
  const listing = await wiki.ls(root);
  const existing = listing.entries?.find((entry) => entry.name === name);
  if (!existing) return "none";
  if (!existing.rev) throw new WikiError("The current revision is unknown, so this file was not updated", 409);
  return existing.rev;
}

export async function importFile(
  root: string,
  file: File,
  update: boolean,
  onProgress: (percent: number) => void,
  operation: PrivateOperation,
): Promise<ImportResult> {
  if (file.name.includes("/") || file.name.includes("\\") || file.name === "." || file.name === "..") {
    throw new WikiError("Use a file name inside this folder", 400);
  }
  const kind = entryKind(file.name);
  const cap = kind === "text" ? MAX_TEXT_BYTES : MAX_FILE_BYTES;
  if (file.size > cap) throw new WikiError(`over ${Math.round(cap / (1024 * 1024))} MB — rejected`, 400);
  const sha256 = await sha256File(file);
  operation.assertCurrent();
  const id = entryId(file.name, 0);
  // Match an interrupted import before re-reading the destination: an
  // interrupted update must resume against the revision it started with,
  // not whatever the file's rev has become since (possibly another editor's).
  const saved = readResume(root).find((item) => item.sha256 === sha256 && item.path === file.name && item.size === file.size && item.update === update);
  const ifRev = saved?.if_rev ?? (await destinationRev(root, file.name, update));
  operation.assertCurrent();
  let jobId = saved?.job_id;
  let digest = saved?.digest;
  if (!jobId) {
    jobId = crypto.randomUUID();
    const manifest = {
      version: 1,
      source_id: "board",
      root,
      entries: [{ id, path: file.name, kind, size: file.size, sha256, if_rev: ifRev }],
    };
    if (JSON.stringify(manifest).length > 256 * 1024) throw new WikiError("wiki transfer manifest exceeds 256KiB", 400);
    const begun = await transferCall(root, { op: "begin", root, job_id: jobId, manifest }, true, operation);
    jobId = text(begun.job_id) ?? "";
    digest = text(begun.digest) ?? "";
    if (!jobId || !digest) throw new WikiError("wiki transfer begin did not return a job", 502);
    writeResume({ job_id: jobId, digest, root, update, id, path: file.name, kind, size: file.size, sha256, if_rev: ifRev });
  }
  const status = await transferCall(root, { op: "status", root, job_id: jobId, offset: 0, limit: 32 }, false, operation);
  const entries = Array.isArray(status.entries) ? status.entries : [];
  const entry = entries.map(record).find((item) => item?.entry_id === id || item?.path === file.name) ?? null;
  let received = whole(entry?.received) ?? 0;
  let outcome = text(entry?.outcome) ?? "pending";
  onProgress(file.size === 0 ? 0 : (received / file.size) * 100);
  if (text(status.state) === "open" && (outcome === "pending" || outcome === "unresolved")) {
    while (received < file.size) {
      const end = Math.min(file.size, received + CHUNK_BYTES);
      const bytes = new Uint8Array(await file.slice(received, end).arrayBuffer());
      operation.assertCurrent();
      const chunk = await transferCall(root, {
        op: "chunk",
        root,
        job_id: jobId,
        entry_id: id,
        offset: received,
        data_base64: bytesBase64(bytes),
      }, true, operation);
      const next = whole(chunk.received);
      if (next === null || next <= received) throw new WikiError("wiki transfer chunk did not advance", 502);
      received = next;
      onProgress((received / file.size) * 100);
    }
    const applied = await transferCall(root, { op: "apply", root, job_id: jobId, entry_id: id }, true, operation);
    outcome = text(applied.outcome) ?? "pending";
    const extraction = text(applied.extraction) ?? "not_applicable";
    const rev = text(applied.rev) ?? undefined;
    const error = text(applied.error) ?? undefined;
    const finished = await transferCall(root, { op: "finish", root, job_id: jobId }, true, operation);
    forgetResume(root, sha256);
    // Only an applied or unchanged file is a success. A conflict or failure
    // must never surface as a done row with "was saved".
    if (outcome !== "applied" && outcome !== "skipped") throw new WikiError(error ?? `The import was not applied (${outcome})`, 502);
    return { outcome, extraction, index: text(finished.index) ?? "pending", ...(rev ? { rev } : {}), ...(error ? { error } : {}) };
  }
  const extraction = text(entry?.extraction) ?? "not_applicable";
  const rev = text(entry?.rev) ?? undefined;
  const error = text(entry?.error) ?? undefined;
  if (text(status.state) === "open") {
    const finished = await transferCall(root, { op: "finish", root, job_id: jobId }, true, operation);
    forgetResume(root, sha256);
    if (outcome !== "applied" && outcome !== "skipped") throw new WikiError(error ?? `The import was not applied (${outcome})`, 502);
    return { outcome, extraction, index: text(finished.index) ?? "pending", ...(rev ? { rev } : {}), ...(error ? { error } : {}) };
  }
  forgetResume(root, sha256);
  if (outcome !== "applied" && outcome !== "skipped") throw new WikiError(error ?? `The import was not applied (${outcome})`, 502);
  return { outcome, extraction, index: text(status.index) ?? "pending", ...(rev ? { rev } : {}), ...(error ? { error } : {}) };
}

/**
 * Hosted boards (whose Worker advertises `wiki: true`) serve no transfer-job
 * protocol: one multipart `POST /api/wiki/upload` carries the file (`file`)
 * and its destination (`path`, plus `if_rev`) — the same shape
 * the local board accepts. Progress is all-or-nothing.
 */
export async function uploadMultipart(
  root: string,
  file: File,
  update: boolean,
  onProgress: (percent: number) => void,
  operation: PrivateOperation,
): Promise<ImportResult> {
  if (file.name.includes("/") || file.name.includes("\\") || file.name === "." || file.name === "..") {
    throw new WikiError("Use a file name inside this folder", 400);
  }
  const cap = entryKind(file.name) === "text" ? MAX_TEXT_BYTES : MAX_FILE_BYTES;
  if (file.size > cap) throw new WikiError(`over ${Math.round(cap / (1024 * 1024))} MB — rejected`, 400);
  const ifRev = await destinationRev(root, file.name, update);
  operation.assertCurrent();
  const form = new FormData();
  form.append("path", root ? `${root}/${file.name}` : file.name);
  form.append("if_rev", ifRev); // "none" = create only, never clobber
  form.append("file", file, file.name);
  onProgress(0);
  const resp = await privateFetch("/api/wiki/upload", {
    method: "POST",
    headers: { "X-Cadence-Board": "1", ...sessionHeaders() },
    body: form,
  }, operation);
  const body: unknown = await resp.json().catch(() => null);
  operation.assertCurrent();
  if (resp.status === 401) notifyPrivateSessionRequired(resp.status, body, operation);
  const row = record(body);
  if (!resp.ok) {
    // Hosted refusals nest as {error:{code,message}}; local ones are {error:"…"}.
    const nested = record(row?.error);
    throw wikiErrorFrom(resp.status, nested ? { ...row, error: text(nested.message), code: text(row?.code) ?? text(nested.code) } : body);
  }
  onProgress(100);
  const rev = text(row?.rev) ?? undefined;
  return { outcome: text(row?.outcome) ?? "applied", extraction: text(row?.extraction) ?? "not_applicable", index: text(row?.index) ?? "covered", ...(rev ? { rev } : {}) };
}

export async function exportOriginal(path: string, at: string, operation: PrivateOperation): Promise<Blob> {
  if (!at) throw new WikiError("This revision is unavailable", 404);
  const parts: Uint8Array[] = [];
  const hash = createSha256();
  let sha256: string | null = null;
  let offset = 0;
  let size: number | null = null;
  for (;;) {
    const slice = await transferCall(path, { op: "export", root: path, path, offset, bytes: CHUNK_BYTES, at, rev: at }, false, operation);
    if (text(slice.rev) !== at) throw new WikiError("This revision is unavailable", 404);
    const digest = text(slice.sha256);
    if (!digest || !/^[a-f0-9]{64}$/.test(digest)) throw new WikiError("The original digest is unavailable", 502);
    if (sha256 === null) sha256 = digest;
    if (sha256 !== digest || whole(slice.offset) !== offset) throw new WikiError("The original is not the pinned revision", 502);
    const declared = whole(slice.size);
    if (declared === null) throw new WikiError("The original size is unknown", 502);
    if (size === null) size = declared;
    if (size !== declared || size > MAX_FILE_BYTES) throw new WikiError("The original size is not the pinned revision", 502);
    const encoded = typeof slice.data_base64 === "string" ? slice.data_base64 : null;
    if (encoded === null) throw new WikiError("The original bytes are unavailable", 502);
    const bytes = decodeBase64(encoded);
    if (bytes.length > CHUNK_BYTES || offset + bytes.length > declared) throw new WikiError("The original range is invalid", 502);
    hash.update(bytes);
    parts.push(bytes);
    offset += bytes.length;
    const next = slice.next_offset;
    if (next === null || next === undefined) break;
    const advanced = whole(next);
    if (advanced === null || advanced !== offset) throw new WikiError("The original read did not advance contiguously", 502);
    offset = advanced;
    if (parts.length > Math.ceil(MAX_FILE_BYTES / CHUNK_BYTES) + 1) throw new WikiError("The original exceeds the transfer cap", 502);
  }
  if (offset !== size || hash.hex() !== sha256) throw new WikiError("The original bytes do not match the pinned digest", 502);
  return new Blob(parts as BlobPart[]);
}
