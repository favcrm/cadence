import type * as ComposerStoreModule from "../src/features/app-shell/chat/composerStore";
import type * as PrivateSessionModule from "../src/lib/privateSession";
import type { ChatFileRow } from "../src/lib/api";
/**
 * A paused private read must abort the upload that is already on the wire
 * and leave both that row and the one waiting behind it retryable. Same-
 * generation resume does not revive the held transfer. A later completion
 * of the paused request must not replace a row that a fresh retry made ready.
 */
declare function require(name: string): unknown;


class UploadRequest {
  status = 0;
  statusText = "";
  responseText = "";
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  method = "";
  url = "";
  readonly headers = new Map<string, string>();
  form: FormData | null = null;
  aborted = false;
  settled = false;
  private opened = false;

  open(method: string, url: string): void {
    this.method = method;
    this.url = url;
    this.opened = true;
  }

  setRequestHeader(key: string, value: string): void {
    this.headers.set(key, value);
  }

  send(body?: Document | XMLHttpRequestBodyInit | null): void {
    if (!this.opened) throw new Error("upload send before open");
    if (!(body instanceof FormData)) throw new Error("chat upload sends FormData");
    this.form = body;
    const file = body.get("file");
    const name = file instanceof File ? file.name : null;
    if (name === null || name === "") throw new Error("chat upload names its file");
    const queued = uploads.get(name) ?? [];
    queued.push(this);
    uploads.set(name, queued);
  }

  abort(): void {
    if (this.aborted) return;
    this.aborted = true;
    this.settled = true;
    this.onabort?.();
  }

  complete(body: unknown): void {
    if (this.aborted || this.settled) return;
    this.settled = true;
    this.status = 200;
    this.statusText = "OK";
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }

  /** A completion that arrives after abort, or after a newer attempt owns the row. */
  deliverLate(body: unknown): void {
    this.status = 200;
    this.statusText = "OK";
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
}

const uploads = new Map<string, UploadRequest[]>();
globalThis.XMLHttpRequest = UploadRequest as unknown as typeof XMLHttpRequest;

const { addFiles, attachItems, retryAttach, setComposerDraft, composerDraftState } =
  require("../src/features/app-shell/chat/composerStore") as typeof ComposerStoreModule;
const { allowPrivateReads, pausePrivateReads, privateSessionGeneration } =
  require("../src/lib/privateSession") as typeof PrivateSessionModule;
const process = require("process") as { exitCode?: number };

const KEY = "app|install-crm|conv-held";
const INSTALL = "install-crm";
const CONTEXT = "ctx-a";
const CONVERSATION = "conv-held";
const DRAFT = "keep this composer text through pause";
const FIRST_NAME = "held-first.txt";
const SECOND_NAME = "queued-next.txt";
const FIRST_ID = `chf-${"11".repeat(16)}`;
const SECOND_ID = `chf-${"22".repeat(16)}`;
const STALE_ID = `chf-${"ff".repeat(16)}`;
const destination = {
  app: { install_id: INSTALL, context_id: CONTEXT },
  conversation: CONVERSATION,
};

const assert: (condition: unknown, message: string) => asserts condition = (condition, message) => {
  if (!condition) throw new Error(message);
};

function eventTurn(): Promise<void> {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      channel.port1.onmessage = null;
      channel.port1.close();
      channel.port2.close();
      resolve();
    };
    channel.port2.postMessage(null);
  });
}

/** A fixed turn count. This must not wait until a queued upload appears. */
async function drain(): Promise<void> {
  await eventTurn();
  await eventTurn();
  await eventTurn();
}

function receipt(id: string, name: string, size: number): ChatFileRow {
  return {
    id,
    sha256: "0123456789abcdef".repeat(4),
    size,
    mime: "text/plain",
    name,
    scope: INSTALL,
    context_id: CONTEXT,
    uploader: "operator",
    user_id: `acu-${"abcdef0123456789".repeat(2)}`,
    thread_id: CONVERSATION,
    location_version: 1,
    created: 1_700_000_000,
  };
}

function requestsNamed(name: string): readonly UploadRequest[] {
  return uploads.get(name) ?? [];
}

function liveRequest(name: string): UploadRequest {
  const request = requestsNamed(name).find((candidate) => !candidate.aborted && !candidate.settled);
  if (request === undefined) throw new Error(`${name} has a live upload`);
  return request;
}

function rowNamed(name: string) {
  const row = attachItems(KEY).find((item) => item.name === name);
  assert(row, `${name} is still in the queue`);
  return row;
}

function formText(form: FormData, key: string): string {
  const value = form.get(key);
  assert(typeof value === "string", `${key} is a text field`);
  return value;
}

function assertDestination(form: FormData, file: File, why: string): void {
  assert(formText(form, "name") === file.name, `${why}: form name`);
  assert(formText(form, "conversation") === CONVERSATION, `${why}: conversation`);
  assert(formText(form, "install_id") === INSTALL, `${why}: install`);
  assert(formText(form, "context_id") === CONTEXT, `${why}: context`);
  const sent = form.get("file");
  assert(sent instanceof File, `${why}: file`);
  assert(sent.name === file.name && sent.size === file.size, `${why}: original file bytes`);
}

async function retryToReady(name: string, file: File, id: string): Promise<void> {
  const itemKey = rowNamed(name).key;
  retryAttach(KEY, itemKey);
  await drain();
  const request = liveRequest(name);
  assert(request.method === "POST" && request.url === "/api/chat/upload", `${name} retries through the chat upload route`);
  assert(request.headers.get("X-Cadence-Board") === "1", `${name} uses the board upload header`);
  if (request.form === null) throw new Error(`${name} retry sends a form`);
  assertDestination(request.form, file, name);
  request.complete(receipt(id, name, file.size));
  await drain();
  const ready = rowNamed(name);
  assert(ready.key === itemKey, `${name} keeps its row across retry`);
  assert(ready.status === "ready" && ready.id === id, `${name} becomes ready from its own completion`);
  assert(ready.file === file, `${name} keeps the original File`);
  const readyDestination = ready.destination;
  assert(readyDestination?.conversation === CONVERSATION, `${name} keeps its conversation`);
  assert(readyDestination?.app?.install_id === INSTALL, `${name} keeps its install`);
  assert(readyDestination?.app?.context_id === CONTEXT, `${name} keeps its context`);
}

async function main(): Promise<void> {
  const firstFile = new File(["alpha brief"], FIRST_NAME, { type: "text/plain" });
  const secondFile = new File(["queued brief"], SECOND_NAME, { type: "text/plain" });
  const generation = privateSessionGeneration();
  setComposerDraft(KEY, DRAFT);
  addFiles(KEY, [firstFile, secondFile], destination);
  const firstKey = rowNamed(FIRST_NAME).key;
  const secondKey = rowNamed(SECOND_NAME).key;
  assert(firstKey !== secondKey, "the two rows have distinct identities");
  await drain();
  const held = liveRequest(FIRST_NAME);
  assert(held.method === "POST" && held.url === "/api/chat/upload", "the held file uses the chat upload route");
  assert(held.headers.get("X-Cadence-Board") === "1", "the held file uses the board upload header");
  if (held.form === null) throw new Error("the held upload captured its form");
  assertDestination(held.form, firstFile, "held upload");
  assert(
    requestsNamed(SECOND_NAME).find((request) => !request.aborted && !request.settled) === undefined,
    "the queued file has not started a transfer while the first upload is held",
  );

  pausePrivateReads();
  await drain();
  allowPrivateReads();
  assert(privateSessionGeneration() === generation, "resume admits the same generation");
  await drain();

  assert(held.aborted, "pause aborts the held upload's XHR");
  const pausedFirst = rowNamed(FIRST_NAME);
  const pausedSecond = rowNamed(SECOND_NAME);
  assert(pausedFirst.key === firstKey && pausedSecond.key === secondKey, "pause keeps both row identities");
  assert(pausedFirst.status === "failed" && pausedFirst.permanent !== true, "the held row becomes retryable");
  assert(typeof pausedFirst.error === "string" && pausedFirst.error.trim() !== "", "the held row explains why retry is needed");
  assert(pausedSecond.status === "failed" && pausedSecond.permanent !== true, "the queued row leaves the upload stall and stays retryable");
  assert(typeof pausedSecond.error === "string" && pausedSecond.error.trim() !== "", "the queued row explains why retry is needed");
  assert(pausedFirst.file === firstFile && pausedSecond.file === secondFile, "pause keeps the original files");
  assert(composerDraftState(KEY).text === DRAFT, "the text draft survives pause and resume");

  held.deliverLate(receipt(STALE_ID, "stale-held.txt", firstFile.size));
  await drain();
  assert(rowNamed(FIRST_NAME).status === "failed" && rowNamed(FIRST_NAME).id === undefined, "a late completion does not make the paused row ready");

  await retryToReady(FIRST_NAME, firstFile, FIRST_ID);
  held.deliverLate(receipt(STALE_ID, "stale-after-retry.txt", firstFile.size));
  await drain();
  assert(rowNamed(FIRST_NAME).status === "ready" && rowNamed(FIRST_NAME).id === FIRST_ID, "the stale held completion does not replace the ready row");

  await retryToReady(SECOND_NAME, secondFile, SECOND_ID);
  held.deliverLate(receipt(STALE_ID, "stale-after-both.txt", firstFile.size));
  await drain();
  assert(rowNamed(FIRST_NAME).id === FIRST_ID && rowNamed(SECOND_NAME).id === SECOND_ID, "a stale completion changes neither ready row");
  assert(rowNamed(FIRST_NAME).file === firstFile && rowNamed(SECOND_NAME).file === secondFile, "ready rows still carry the original files");
  assert(composerDraftState(KEY).text === DRAFT, "the text draft is still there after both retries");
  console.log("composerUploadRecovery.test.ts: passed");
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
