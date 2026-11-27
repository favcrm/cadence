import { errorMessage } from "../../lib/apiError";
import { withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import type { DocumentFile, DocumentState } from "../documents/documentsClient";

export type WikiManagedFile = {
  file: DocumentFile;
  document: DocumentState | null;
  actions: { approve: boolean; archive: boolean; restore: boolean };
};

function parse(value: unknown): WikiManagedFile {
  if (!value || typeof value !== "object") throw new Error("Invalid managed file response.");
  const row = value as WikiManagedFile;
  if (!row.file || typeof row.file.id !== "string" || !Number.isSafeInteger(row.file.version) || row.file.version < 1 || typeof row.file.title !== "string" || !row.actions || typeof row.actions.approve !== "boolean" || typeof row.actions.archive !== "boolean" || typeof row.actions.restore !== "boolean") throw new Error("Invalid managed file response.");
  return row;
}

export class ManagedFileError extends Error {
  constructor(readonly status: number, message: string) { super(message); }
}

async function get(path: string, signal?: AbortSignal): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await operation.fetch(path, { headers: sessionHeaders(), cache: "no-store" });
    const body: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw new ManagedFileError(response.status, errorMessage(body, `The board answered ${response.status}.`));
    return body;
  }, signal);
}

export async function listManagedFiles(
  filter: (({ path: string } | { scope: "protected" }) & { archived?: boolean; q?: string }) | { issue: string },
  signal?: AbortSignal,
): Promise<WikiManagedFile[]> {
  const q = new URLSearchParams();
  if ("issue" in filter) q.set("issue", filter.issue);
  else {
    if ("scope" in filter) q.set("scope", filter.scope);
    else q.set("path", filter.path);
    if (filter.archived) q.set("archived", "1");
    if (filter.q) q.set("q", filter.q);
  }
  const body = await get(`/api/wiki/managed-files?${q}`, signal) as { files?: unknown };
  if (!Array.isArray(body?.files)) throw new Error("Invalid managed file list.");
  return body.files.map(parse);
}

export async function getManagedFile(id: string, version: number, signal?: AbortSignal): Promise<WikiManagedFile> {
  const body = await get(`/api/wiki/managed-files/${encodeURIComponent(id)}/${version}`, signal) as { file?: unknown };
  const row = parse(body?.file);
  if (row.file.id !== id || row.file.version !== version) throw new Error("The requested file version is unavailable.");
  return row;
}
