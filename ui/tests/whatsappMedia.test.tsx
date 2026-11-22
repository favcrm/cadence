import type { ScreenActions, MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";
import type { WhatsAppReplySelection } from "../src/features/workspace-apps/screen/WhatsAppReplyComposer";

declare function require(name: string): any;
export {};

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

function sha256HexSync(bytes: Uint8Array): string {
  return require("node:crypto").createHash("sha256").update(bytes).digest("hex");
}

const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/apps/waba" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLTextAreaElement",
  "HTMLButtonElement",
  "HTMLImageElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
  "sessionStorage",
  "localStorage",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "ResizeObserver", {
  value: class {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
  configurable: true,
  writable: true,
});
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });

const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};

const React = require("react") as typeof import("react");
const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const Protocol = require("../src/features/workspace-apps/screen/screenProtocol") as typeof import("../src/features/workspace-apps/screen/screenProtocol");
const Lifecycle = require("../src/features/workspace-apps/screen/screenLifecycle") as typeof import("../src/features/workspace-apps/screen/screenLifecycle");
const Services = require("../src/features/workspace-apps/screen/screenServices") as typeof import("../src/features/workspace-apps/screen/screenServices");
const WhatsAppReplyComposer = (
  require("../src/features/workspace-apps/screen/WhatsAppReplyComposer") as typeof import("../src/features/workspace-apps/screen/WhatsAppReplyComposer")
).default;

const { parseChild, SCREEN_WHATSAPP_MEDIA_V1, validLinkedMediaId } = Protocol;
const { admitsWhatsAppMediaDownload, ScreenChannel } = Lifecycle;
const { recordServices } = Services;

function trustedClick(el: HTMLElement): void {
  const down = new win.MouseEvent("pointerdown", { button: 0, bubbles: true });
  Object.defineProperty(down, "isTrusted", { value: true, configurable: true });
  const click = new win.MouseEvent("click", { detail: 1, bubbles: true });
  Object.defineProperty(click, "isTrusted", { value: true, configurable: true });
  el.dispatchEvent(down);
  el.dispatchEvent(click);
}

function setTextarea(el: HTMLTextAreaElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!;
  setter.call(el, value);
  el.dispatchEvent(new win.Event("input", { bubbles: true }));
}

function pickInputFile(el: HTMLInputElement, file: File): void {
  Object.defineProperty(el, "files", { value: [file], configurable: true });
  el.dispatchEvent(new win.Event("change", { bubbles: true }));
}

async function flush(turns = 8): Promise<void> {
  for (let i = 0; i < turns; i++) {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  }
}

async function testProtocolAndChannel(): Promise<void> {
  assert(validLinkedMediaId("msg-1") && !validLinkedMediaId("") && !validLinkedMediaId("a\nb"), "validLinkedMediaId enforces non-empty printable strings");

  const accepts = [
    "screen.v2",
    "screen-records.v1",
    "screen-locale.v1",
    "screen-whatsapp-compose.v1",
    "screen-whatsapp-reply-availability.v1",
    "screen-whatsapp-reply-layout.v2",
    SCREEN_WHATSAPP_MEDIA_V1,
  ];
  const parsedReady = parseChild({ v: 1, op: "ready", accepts });
  assert(parsedReady && parsedReady.op === "ready" && parsedReady.accepts?.length === 7, "7-token accepts including screen-whatsapp-media.v1 parses");

  const parsedReq = parseChild({ v: 2, op: "whatsapp-media", id: "req1", conversation_id: "15550001111@s.whatsapp.net", message_id: "m1" });
  assert(parsedReq && parsedReq.op === "whatsapp-media" && parsedReq.message_id === "m1", "valid whatsapp-media request parses");
  const parsedInline = parseChild({ v: 2, op: "whatsapp-media", id: "req2", conversation_id: "15550001111@s.whatsapp.net", message_id: "m1", disposition: "inline" });
  assert(parsedInline && parsedInline.op === "whatsapp-media" && parsedInline.disposition === "inline", "inline whatsapp-media request parses");
  const parsedDownload = parseChild({ v: 2, op: "whatsapp-media", id: "req3", conversation_id: "15550001111@s.whatsapp.net", message_id: "m1", disposition: "download" });
  assert(parsedDownload && parsedDownload.op === "whatsapp-media" && parsedDownload.disposition === "download", "download whatsapp-media request parses");
  assert(parseChild({ v: 2, op: "whatsapp-media", id: "req4", conversation_id: "1555@s.whatsapp.net", message_id: "m1", disposition: "evil" }) === null, "invalid disposition refused");
  assert(parseChild({ v: 2, op: "whatsapp-media", id: "req1", conversation_id: "bad\nid", message_id: "m1" }) === null, "control char id refused");
  assert(parseChild({ v: 2, op: "whatsapp-media", id: "req1", conversation_id: "1555@s.whatsapp.net", message_id: "m1", extra: 1 }) === null, "extra key refused");

  const mountFrame = (opts: {
    app?: string;
    ops?: string[];
    accepts?: string[];
    abortedSignals?: AbortSignal[];
    resolvers?: ((v: any) => void)[];
    downloadResult?: { mimeType: string; fileName: string | null };
    onSave?: (bytes: ArrayBuffer, mimeType: string, fileName: string) => void;
  }) => {
    const nonce = "a".repeat(64);
    const digest = `sha256:${"c".repeat(64)}`;
    const receipt: MountReceipt = {
      mount: `/api/app-screen/${"b".repeat(64)}`,
      bridge_nonce: nonce,
      generation: 1,
      tag: "waba",
      action_token: "d".repeat(64),
      tools: [],
      host_contract: "screen-records.v1",
      operations: opts.ops ?? ["whatsapp.messages"],
      navigation: null,
      install_id: "install-waba",
      context_id: "ctx-1",
      digest,
      display_scope_key: "e".repeat(64),
      display_session_key: "f".repeat(64),
      remote_images: [],
    };
    assert(admitsWhatsAppMediaDownload(receipt) === (receipt.operations?.includes("whatsapp.messages") ?? false), "admitsWhatsAppMediaDownload checks whatsapp.messages");
    const projection: ScreenPush = {
      v: 2,
      op: "screen",
      install_id: receipt.install_id!,
      digest,
      tag: "waba",
      context_id: receipt.context_id!,
      installation: {
        install_id: receipt.install_id!,
        name: opts.app ?? "waba",
        title: "WABA",
        version: "0.3.17",
        digest,
        summary: "",
      },
      contexts: [{ id: receipt.context_id!, label: "ctx" }],
      runs: [],
      outbox: [],
      now: 1,
      publish_intents: { status: "ok", withheld: 0, rows: [] },
      actions: receipt.operations,
    };
    let ready = 0;
    let failed = 0;
    const sampleBytes = new TextEncoder().encode("PNG-DATA").buffer;
    const actions: ScreenActions = {
      call: async () => ({ ok: true, data: null }),
      planner: async () => ({ code: "forbidden", text: "no" }),
      onSlot: () => {},
      onLink: () => {},
      onWhatsAppMediaDownload: async (_conv, _msg, signal) => {
        opts.abortedSignals?.push(signal);
        if (opts.resolvers) {
          return await new Promise((resolve) => {
            opts.resolvers!.push(resolve);
          });
        }
        return {
          ok: true,
          bytes: sampleBytes.slice(0),
          mimeType: opts.downloadResult?.mimeType ?? "image/png",
          fileName: opts.downloadResult ? opts.downloadResult.fileName : "shot.png",
          sha256: "1".repeat(64),
        };
      },
      ...(opts.onSave ? { onWhatsAppMediaSave: opts.onSave } : {}),
    };
    const source = {} as Window;
    const channel = new ScreenChannel(source, receipt, projection, () => {}, () => { failed += 1; }, () => { ready += 1; }, undefined, actions);
    const pair = new MessageChannel();
    const replies: Record<string, unknown>[] = [];
    pair.port2.onmessage = (event) => { replies.push(event.data as Record<string, unknown>); };
    pair.port2.start();
    channel.receive({ source, origin: "null", data: { v: 1, op: "init", tag: "waba", bridge_nonce: nonce, generation: 1 }, ports: [pair.port1] });
    pair.port2.postMessage({ v: 1, op: "ready", accepts: opts.accepts ?? ["screen.v2", "screen-records.v1", SCREEN_WHATSAPP_MEDIA_V1] });
    return {
      channel,
      port: pair.port2,
      replies,
      state: () => ({ ready, failed }),
      close: () => { channel.close(); pair.port2.close(); },
    };
  };

  // Admitted read-only member frame transfers ArrayBuffer cleanly
  const member = mountFrame({ ops: ["whatsapp.messages"] });
  try {
    await flush();
    assert(member.state().ready === 1 && member.state().failed === 0, "member frame with whatsapp.messages becomes ready");
    member.port.postMessage({ v: 2, op: "whatsapp-media", id: "dl1", conversation_id: "1555@s.whatsapp.net", message_id: "m1" });
    await flush();
    const reply = member.replies.find((m) => m.op === "whatsapp-media-reply");
    assert(reply && reply.ok === true && reply.bytes instanceof ArrayBuffer && new TextDecoder().decode(reply.bytes) === "PNG-DATA" && reply.mime_type === "image/png" && reply.file_name === "shot.png", "ArrayBuffer transfers via whatsapp-media-reply");
  } finally {
    member.close();
  }

  // Host-mediated save disposition: missing save handler refuses in-place; present handler saves explicit and fallback filenames
  const noSave = mountFrame({ ops: ["whatsapp.messages"] });
  try {
    await flush();
    noSave.port.postMessage({ v: 2, op: "whatsapp-media", id: "sv0", conversation_id: "1555@s.whatsapp.net", message_id: "m1", disposition: "download" });
    await flush();
    const noSaveReply = noSave.replies.find((m) => m.op === "whatsapp-media-reply" && m.id === "sv0");
    assert(noSaveReply && noSaveReply.ok === false && (noSaveReply.refusal as { code: string }).code === "unavailable" && noSave.state().failed === 0, "missing onWhatsAppMediaSave refuses in place");
  } finally {
    noSave.close();
  }

  const savedCalls: { bytes: ArrayBuffer; mimeType: string; fileName: string }[] = [];
  const withSave = mountFrame({
    ops: ["whatsapp.messages"],
    downloadResult: { mimeType: "application/pdf", fileName: "invoice.pdf" },
    onSave: (bytes, mimeType, fileName) => savedCalls.push({ bytes, mimeType, fileName }),
  });
  try {
    await flush();
    withSave.port.postMessage({ v: 2, op: "whatsapp-media", id: "sv1", conversation_id: "1555@s.whatsapp.net", message_id: "m1", disposition: "download" });
    await flush();
    const saveReply = withSave.replies.find((m) => m.op === "whatsapp-media-reply" && m.id === "sv1");
    assert(
      saveReply && saveReply.ok === true && saveReply.saved === true && !("bytes" in saveReply) &&
        saveReply.file_name === "invoice.pdf" && saveReply.mime_type === "application/pdf" &&
        savedCalls.length === 1 && savedCalls[0]!.fileName === "invoice.pdf" && savedCalls[0]!.mimeType === "application/pdf",
      "download disposition triggers host save with explicit filename and returns saved metadata",
    );
  } finally {
    withSave.close();
  }

  const fallbackSaveCalls: { fileName: string }[] = [];
  const fallbackSave = mountFrame({
    ops: ["whatsapp.messages"],
    downloadResult: { mimeType: "audio/ogg", fileName: null },
    onSave: (_bytes, _mimeType, fileName) => fallbackSaveCalls.push({ fileName }),
  });
  try {
    await flush();
    fallbackSave.port.postMessage({ v: 2, op: "whatsapp-media", id: "sv2", conversation_id: "1555@s.whatsapp.net", message_id: "m2", disposition: "download" });
    await flush();
    const fbReply = fallbackSave.replies.find((m) => m.op === "whatsapp-media-reply" && m.id === "sv2");
    assert(fbReply && fbReply.ok === true && fbReply.saved === true && fbReply.file_name === "whatsapp-audio.ogg" && fallbackSaveCalls[0]?.fileName === "whatsapp-audio.ogg", "download disposition derives fallback filename when header filename is null");
  } finally {
    fallbackSave.close();
  }

  // Unnegotiated, non-WABA, and missing whatsapp.messages frames are refused / closed
  const noCap = mountFrame({ accepts: ["screen.v2", "screen-records.v1"] });
  try {
    await flush();
    assert(noCap.state().ready === 1, "frame without SCREEN_WHATSAPP_MEDIA_V1 still mounts");
    noCap.port.postMessage({ v: 2, op: "whatsapp-media", id: "dl1", conversation_id: "1555@s.whatsapp.net", message_id: "m1" });
    await flush();
    assert(noCap.state().failed === 1, "unnegotiated frame closes on whatsapp-media");
  } finally {
    noCap.close();
  }

  const wrongApp = mountFrame({ app: "crm" });
  try {
    await flush();
    assert(wrongApp.state().failed === 1 && wrongApp.state().ready === 0, "non-WABA frame cannot negotiate screen-whatsapp-media.v1");
  } finally {
    wrongApp.close();
  }

  const missingOp = mountFrame({ ops: ["whatsapp.status"] });
  try {
    await flush();
    assert(missingOp.state().failed === 1 && missingOp.state().ready === 0, "mount without whatsapp.messages cannot negotiate screen-whatsapp-media.v1");
  } finally {
    missingOp.close();
  }

  // Concurrent download cap (>4 returns busy) and abort on channel.close()
  const resolvers: ((v: any) => void)[] = [];
  const signals: AbortSignal[] = [];
  const busyFrame = mountFrame({ resolvers, abortedSignals: signals });
  try {
    await flush();
    for (let i = 1; i <= 5; i++) {
      busyFrame.port.postMessage({ v: 2, op: "whatsapp-media", id: `dl${i}`, conversation_id: "1555@s.whatsapp.net", message_id: `m${i}` });
    }
    await flush();
    const busyMsg = busyFrame.replies.find((m) => m.op === "whatsapp-media-reply" && m.id === "dl5");
    assert(busyMsg && busyMsg.ok === false && (busyMsg.refusal as { code: string }).code === "busy", "5th concurrent download refuses with busy");
    busyFrame.close();
    assert(signals.length === 4 && signals.every((s) => s.aborted), "closing channel aborts all in-flight download signals");
  } finally {
    busyFrame.close();
  }
}

async function testScreenServicesMedia(): Promise<void> {
  const attId = `wam_${"a".repeat(32)}`;
  const receipt: MountReceipt = {
    mount: `/api/app-screen/${"b".repeat(64)}`,
    bridge_nonce: "a".repeat(64),
    generation: 1,
    tag: "waba",
    action_token: "d".repeat(64),
    tools: [],
    host_contract: "screen-records.v1",
    operations: ["whatsapp.messages", "whatsapp.reply.prepare", "whatsapp.reply.send"],
    navigation: null,
    install_id: "install-waba",
    context_id: "ctx-1",
    digest: `sha256:${"c".repeat(64)}`,
    display_scope_key: "e".repeat(64),
    display_session_key: "f".repeat(64),
    remote_images: [],
  };

  let retiredCount = 0;
  const services = recordServices(receipt, "media-domain", {
    current: () => retiredCount === 0,
    refused: () => { retiredCount += 1; },
    command: () => {},
  });

  const samplePng = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]);
  const sampleSha = sha256HexSync(samplePng);
  const realFetch = globalThis.fetch;
  let nextResponse!: () => Response;
  let lastRequestHeaders: Headers | undefined;
  globalThis.fetch = (async (_input: any, init?: RequestInit) => {
    lastRequestHeaders = new Headers(init?.headers);
    return nextResponse();
  }) as unknown as typeof fetch;

  try {
    // Confirmation card via planner("whatsapp.reply.send", ...): text-only, attachment, and malformed
    const token = "1".repeat(64);
    nextResponse = () =>
      Response.json({
        confirmation_token: token,
        card: {
          operation: "whatsapp.reply.send",
          facts: {
            recipient: { name: "Ada", phone: "+15550001" },
            conversation_id: "15550001@s.whatsapp.net",
            text: "Hello",
            digest: "a".repeat(64),
          },
        },
      });
    const textPlan = await services.planner("whatsapp.reply.send", {
      provider: "linked",
      effect_id: `wae_${"1".repeat(32)}`,
      conversation_id: "15550001@s.whatsapp.net",
      text: "Hello",
    });
    assert(
      "recordCard" in textPlan &&
        !!textPlan.recordCard?.facts.some((f) => f.label === "Message" && f.value === "Hello") &&
        !textPlan.recordCard?.facts.some((f) => f.label === "Attachment"),
      "text-only reply card preserves exact rows",
    );

    nextResponse = () =>
      Response.json({
        confirmation_token: token,
        card: {
          operation: "whatsapp.reply.send",
          facts: {
            recipient: { name: null, phone: "+15550001" },
            conversation_id: "15550001@s.whatsapp.net",
            text: "",
            digest: "a".repeat(64),
            attachments: [
              {
                attachment_id: attId,
                kind: "audio",
                mime_type: "audio/ogg",
                file_name: null,
                byte_length: 2048,
                sha256: "b".repeat(64),
              },
            ],
          },
        },
      });
    const attPlan = await services.planner("whatsapp.reply.send", {
      provider: "linked",
      effect_id: `wae_${"1".repeat(32)}`,
      conversation_id: "15550001@s.whatsapp.net",
      text: "",
    });
    assert("recordCard" in attPlan && !!attPlan.recordCard, "attachment plan built");
    assert(!attPlan.recordCard.facts.some((f) => f.label === "Message"), "empty audio caption omits Message row");
    assert(
      attPlan.recordCard.facts.some((f) => f.label === "Attachment" && f.value === "audio (audio, audio/ogg, 2 KB)"),
      "Attachment row formatted",
    );
    assert(
      attPlan.recordCard.facts.some((f) => f.label === "Attachment SHA-256" && f.value === "b".repeat(64)),
      "Attachment SHA-256 row shown",
    );

    nextResponse = () =>
      Response.json({
        confirmation_token: token,
        card: {
          operation: "whatsapp.reply.send",
          facts: {
            recipient: { name: null, phone: null },
            conversation_id: "15550001@s.whatsapp.net",
            text: "hi",
            digest: "a".repeat(64),
            attachments: [{ attachment_id: "bad-id" }],
          },
        },
      });
    const badPlan = await services.planner("whatsapp.reply.send", {
      provider: "linked",
      effect_id: `wae_${"1".repeat(32)}`,
      conversation_id: "15550001@s.whatsapp.net",
      text: "hi",
    });
    assert("code" in badPlan, "malformed facts.attachments refuses plan");

    const sampleFile = new File([samplePng], "shot.png", { type: "image/png" });

    // stage 200 -> does not set manual Content-Type, parses exact receipt
    nextResponse = () =>
      Response.json({
        attachment_id: attId,
        kind: "image",
        mime_type: "image/png",
        file_name: null,
        byte_length: samplePng.byteLength,
        sha256: sampleSha,
        expires_at: "2099-01-01T00:00:00Z",
      });
    const staged = await services.stageWhatsAppMedia(sampleFile);
    assert(staged.ok && staged.data.attachment_id === attId && !lastRequestHeaders?.has("content-type"), "stageWhatsAppMedia returns receipt without manual Content-Type");

    // stage 404 / 413 / 415 / 409 / 403 operation_not_declared must NOT retire mount
    for (const [status, expectedCode, body] of [
      [404, "unsupported_host", { error: { code: "not_found", message: "Not found" } }],
      [413, "payload_too_large", { error: { code: "payload_too_large", message: "Too big" } }],
      [415, "unsupported_media_type", { error: { code: "unsupported_media_type", message: "Bad type" } }],
      [409, "owner_required", { error: { code: "owner_required", message: "Owner only" } }],
      [403, "forbidden", { error: { code: "operation_not_declared", message: "Missing op" } }],
    ] as const) {
      nextResponse = () => Response.json(body, { status });
      const err = await services.stageWhatsAppMedia(sampleFile);
      assert(!err.ok && err.refusal.code === expectedCode && retiredCount === 0, `stage ${status} maps to ${expectedCode} without retiring mount`);
    }

    // readStagedWhatsAppMedia checks SHA-256
    nextResponse = () =>
      new Response(samplePng, {
        status: 200,
        headers: { "content-type": "image/png", "x-linked-media-sha256": sampleSha },
      });
    const previewOk = await services.readStagedWhatsAppMedia(attId);
    assert(previewOk.ok && previewOk.sha256 === sampleSha, "readStagedWhatsAppMedia returns verified Blob");

    nextResponse = () =>
      new Response(samplePng, {
        status: 200,
        headers: { "content-type": "image/png", "x-linked-media-sha256": "0".repeat(64) },
      });
    const previewBad = await services.readStagedWhatsAppMedia(attId);
    assert(!previewBad.ok, "readStagedWhatsAppMedia rejects SHA mismatch");

    // deleteStagedWhatsAppMedia 200
    nextResponse = () => Response.json({ deleted: true }, { status: 200 });
    const del = await services.deleteStagedWhatsAppMedia(attId);
    assert(del.ok, "deleteStagedWhatsAppMedia succeeds");

    // downloadWhatsAppMedia 200, SVG refusal, SHA mismatch refusal, 404/409/422 without retiring mount
    nextResponse = () =>
      new Response(samplePng, {
        status: 200,
        headers: {
          "content-type": "image/png",
          "content-disposition": 'inline; filename="image.png"',
          "x-linked-media-sha256": sampleSha,
        },
      });
    const dlOk = await services.downloadWhatsAppMedia("1555@s.whatsapp.net", "m1");
    assert(dlOk.ok && dlOk.fileName === "image.png" && dlOk.bytes.byteLength === samplePng.byteLength, "downloadWhatsAppMedia verifies bytes and filename");

    nextResponse = () =>
      new Response(new TextEncoder().encode("<svg></svg>"), {
        status: 200,
        headers: { "content-type": "image/svg+xml", "x-linked-media-sha256": sampleSha },
      });
    assert(!(await services.downloadWhatsAppMedia("1555@s.whatsapp.net", "m1")).ok, "downloadWhatsAppMedia rejects SVG");

    nextResponse = () =>
      new Response(samplePng, {
        status: 200,
        headers: { "content-type": "image/png", "x-linked-media-sha256": "f".repeat(64) },
      });
    assert(!(await services.downloadWhatsAppMedia("1555@s.whatsapp.net", "m1")).ok, "downloadWhatsAppMedia rejects SHA mismatch");

    for (const [status, code] of [
      [404, "expired_on_provider"],
      [409, "device_offline"],
      [422, "too_large"],
    ] as const) {
      nextResponse = () => Response.json({ error: { code, message: code } }, { status });
      const dlErr = await services.downloadWhatsAppMedia("1555@s.whatsapp.net", "m1");
      assert(!dlErr.ok && dlErr.refusal.code === code && retiredCount === 0, `download ${status} preserves ${code} without retiring`);
    }

    // 401 retires mount
    nextResponse = () => Response.json({ error: { code: "unauthorized", message: "Expired" } }, { status: 401 });
    const unauth = await services.stageWhatsAppMedia(sampleFile);
    assert(!unauth.ok && retiredCount === 1, "401 retires mount");
  } finally {
    globalThis.fetch = realFetch;
  }
}

async function testComposerInteractions(): Promise<void> {
  const conv = "15550000123@s.whatsapp.net";
  const workRef = `lw_${"1".repeat(32)}_${"2".repeat(64)}`;
  const pdfAttId = `wam_${"a".repeat(32)}`;
  const pngAttId = `wam_${"b".repeat(32)}`;
  const oggAttId = `wam_${"c".repeat(32)}`;
  const effectId = `wae_${"9".repeat(32)}`;
  const pngBytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2]);
  const pngSha = sha256HexSync(pngBytes);
  const pdfSha = "e".repeat(64);
  const oggSha = "f".repeat(64);

  const callArgs: { op: string; args: Record<string, unknown> }[] = [];
  const plannerArgs: Record<string, unknown>[] = [];
  const deletedIds: string[] = [];
  let stageCalls = 0;
  let prepareMode: "ok" | "facts_changed" = "ok";
  let confirmOutcome: "failed" | "uncertain" | "accepted" = "failed";

  let statusOutcome: "sent" | "accepted" | "uncertain" = "sent";

  const mockServices = {
    async call(op: string, args: Record<string, unknown>) {
      callArgs.push({ op, args });
      if (op === "whatsapp.conversations.show") {
        return {
          ok: true as const,
          data: {
            contract: "agenticos.whatsapp-door.v1",
            conversationId: conv,
            workRef,
            contact: { name: "Alex", phone: "+15550000123" },
          },
        };
      }
      if (op === "whatsapp.reply.prepare") {
        if (prepareMode === "facts_changed") {
          return { ok: false as const, refusal: { code: "facts_changed", text: "Attachment changed." } };
        }
        return {
          ok: true as const,
          data: { decision: "prepared", effect_id: effectId, expires_at: "2099-01-01T00:00:00Z" },
        };
      }
      return { ok: true as const, data: { state: statusOutcome } };
    },
    async planner(op: string, args: Record<string, unknown>) {
      assert(op === "whatsapp.reply.send", "planner called for whatsapp.reply.send");
      plannerArgs.push(args);
      return {
        label: "Send",
        records: true,
        run: async () => ({ ok: true as const, data: { reply: { state: confirmOutcome } } }),
      };
    },
    async stageWhatsAppMedia(file: File) {
      stageCalls += 1;
      if (file.type === "application/pdf") {
        return {
          ok: true as const,
          data: {
            attachment_id: pdfAttId,
            kind: "document" as const,
            mime_type: "application/pdf",
            file_name: file.name,
            byte_length: file.size,
            sha256: pdfSha,
            expires_at: "2099-01-01T00:00:00Z",
          },
        };
      }
      if (file.type === "image/png") {
        return {
          ok: true as const,
          data: {
            attachment_id: pngAttId,
            kind: "image" as const,
            mime_type: "image/png",
            file_name: null,
            byte_length: file.size,
            sha256: pngSha,
            expires_at: "2099-01-01T00:00:00Z",
          },
        };
      }
      return {
        ok: true as const,
        data: {
          attachment_id: oggAttId,
          kind: "audio" as const,
          mime_type: "audio/ogg",
          file_name: null,
          byte_length: file.size,
          sha256: oggSha,
          expires_at: "2099-01-01T00:00:00Z",
        },
      };
    },
    async readStagedWhatsAppMedia(_attachmentId: string) {
      return { ok: true as const, blob: new Blob([pngBytes], { type: "image/png" }), mimeType: "image/png", sha256: pngSha };
    },
    async deleteStagedWhatsAppMedia(attachmentId: string) {
      deletedIds.push(attachmentId);
      return { ok: true as const };
    },
  };

  const dirtyStates: boolean[] = [];
  const selection: WhatsAppReplySelection = {
    conversationId: conv,
    receiptScope: "scope-media",
    services: mockServices as unknown as WhatsAppReplySelection["services"],
    current: () => true,
    announce: (dirty) => { dirtyStates.push(dirty); },
  };

  const realCreateObjectURL = URL.createObjectURL;
  const realRevokeObjectURL = URL.revokeObjectURL;
  URL.createObjectURL = () => "blob:mock-preview";
  URL.revokeObjectURL = () => {};

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  try {
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { selection })));
      await flush();
    });

    const fInput = container.querySelector('input[type="file"]') as HTMLInputElement;
    const tArea = container.querySelector("textarea") as HTMLTextAreaElement;
    const sendBtn = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    assert(fInput && tArea && sendBtn, "composer controls rendered");

    // 0. Client-side MIME/extension allowlist & size checks refuse before staging
    const fakeSizedFile = (name: string, type: string, size: number) => {
      const f = new File(["x"], name, { type });
      Object.defineProperty(f, "size", { value: size, configurable: true });
      return f;
    };
    const stageBefore = stageCalls;
    for (const [badFile, expectedMsg] of [
      [new File(["<svg/>"], "bad.svg", { type: "image/svg+xml" }), "not supported"],
      [new File(["MZ"], "run.exe", { type: "" }), "not supported"],
      [fakeSizedFile("huge.png", "image/png", 5 * 1024 * 1024 + 1), "5 MB or smaller"],
      [fakeSizedFile("huge.pdf", "application/pdf", 10 * 1024 * 1024 + 1), "10 MB or smaller"],
    ] as const) {
      await React.act(async () => {
        pickInputFile(fInput, badFile);
        await flush();
      });
      assert(
        stageCalls === stageBefore &&
          !container.querySelector(".wa-reply-attachment") &&
          container.textContent?.includes(expectedMsg),
        `client-side validation refuses ${badFile.name} (${expectedMsg}) before staging`,
      );
    }

    // 1. Attach PDF, set caption, failed confirm keeps both attachment and text
    await React.act(async () => {
      pickInputFile(fInput, new File(["%PDF-1.7\nhi"], "invoice.pdf", { type: "application/pdf" }));
      setTextarea(tArea, "Invoice PDF");
      await flush();
    });
    assert(container.querySelector(".wa-reply-att-name")?.textContent === "invoice.pdf", "PDF chip rendered");

    await React.act(async () => {
      trustedClick(sendBtn);
      await flush();
    });
    const prepCall = callArgs.find((c) => c.op === "whatsapp.reply.prepare");
    assert(
      prepCall && JSON.stringify(prepCall.args.attachments) === JSON.stringify([{ attachment_id: pdfAttId, sha256: pdfSha }]),
      "prepare includes staged attachment ref",
    );
    assert(
      JSON.stringify(plannerArgs[0]?.attachments) === JSON.stringify([{ attachment_id: pdfAttId, sha256: pdfSha }]),
      "planner includes staged attachment ref",
    );
    assert(container.querySelector(".wa-reply-att-name")?.textContent === "invoice.pdf" && tArea.value === "Invoice PDF", "failed confirm keeps attachment and caption for retry");

    // 2. Remove attachment calls deleteStagedWhatsAppMedia
    const removeBtn = container.querySelector("button.wa-reply-att-remove") as HTMLButtonElement;
    await React.act(async () => {
      removeBtn.click();
      await flush();
    });
    assert(deletedIds.includes(pdfAttId) && !container.querySelector(".wa-reply-attachment"), "remove button deletes staged attachment");

    // 3. Stage PNG -> preview img -> facts_changed clears stale attachment, keeps caption
    await React.act(async () => {
      pickInputFile(fInput, new File([pngBytes], "photo.png", { type: "image/png" }));
      await flush();
    });
    assert(container.querySelector("img.wa-reply-thumb")?.getAttribute("src") === "blob:mock-preview", "image thumbnail renders");

    prepareMode = "facts_changed";
    await React.act(async () => {
      trustedClick(sendBtn);
      await flush();
    });
    assert(!container.querySelector(".wa-reply-attachment") && tArea.value === "Invoice PDF" && container.textContent?.includes("Re-attach"), "facts_changed clears stale attachment while keeping caption");

    // 4. Stage PNG again, uncertain confirm clears both text and attachment without deleting already-sent slot ref
    prepareMode = "ok";
    confirmOutcome = "uncertain";
    const deletesBeforeUncertain = deletedIds.length;
    await React.act(async () => {
      pickInputFile(fInput, new File([pngBytes], "photo2.png", { type: "image/png" }));
      await flush();
    });
    await React.act(async () => {
      trustedClick(sendBtn);
      await flush();
    });
    assert(!container.querySelector(".wa-reply-attachment") && String(tArea.value) === "" && deletedIds.length === deletesBeforeUncertain, "uncertain outcome clears draft and does not delete via UI");
    assert(dirtyStates.includes(true) && dirtyStates.at(-1) === false, "dirty state announcements reflect draft and submit");

    // Clear uncertain lock by remounting a fresh scope for audio rule check
    const audioSelection: WhatsAppReplySelection = {
      ...selection,
      receiptScope: "scope-audio",
    };
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "audio", selection: audioSelection })));
      await flush();
    });
    const audioFileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
    const audioText = container.querySelector("textarea") as HTMLTextAreaElement;
    const audioSendBtn = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    await React.act(async () => {
      setTextarea(audioText, "caption not allowed");
      pickInputFile(audioFileInput, new File(["OggS"], "voice.ogg", { type: "audio/ogg" }));
      await flush();
    });
    assert(audioSendBtn.disabled && container.textContent?.includes("can’t include a caption"), "audio with caption disables send and warns");
    await React.act(async () => {
      setTextarea(audioText, "");
      await flush();
    });
    assert(!audioSendBtn.disabled && audioText.readOnly, "audio with empty caption enables send and locks caption input");

    // 5. Bounded auto-poll resolves transient uncertain -> sent without alert lock
    statusOutcome = "sent";
    confirmOutcome = "uncertain";
    const autoPollSelection: WhatsAppReplySelection = {
      ...selection,
      receiptScope: "scope-autopoll",
      announceInset: () => 96,
    };
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "autopoll", selection: autoPollSelection, bounds: [0, 10000], inset: { px: 96, seq: 1 } })));
      await flush();
    });
    const apText = container.querySelector("textarea") as HTMLTextAreaElement;
    const apSend = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    await React.act(async () => {
      setTextarea(apText, "Auto poll confirm");
      await flush();
    });
    await React.act(async () => {
      trustedClick(apSend);
      await flush();
    });
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "confirming" &&
        container.textContent?.includes("Confirming send") &&
        !container.textContent?.includes("Send not confirmed") &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") !== "true" &&
        apSend.disabled,
      "initial uncertain starts confirming state without locking card or showing alert immediately",
    );
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 700));
      await flush();
    });
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "sent" &&
        container.textContent?.includes("Delivery confirmed") &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") !== "true",
      "bounded auto-poll promotes transient uncertain to sent and unlocks composer",
    );

    // 6. Persistent uncertain escalates to Send not confirmed + lock + manual Check status unlocks
    statusOutcome = "uncertain";
    const escalateSelection: WhatsAppReplySelection = {
      ...selection,
      receiptScope: "scope-escalate",
      announceInset: () => 96,
    };
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "escalate", selection: escalateSelection, bounds: [0, 10000], inset: { px: 96, seq: 1 } })));
      await flush();
    });
    const esText = container.querySelector("textarea") as HTMLTextAreaElement;
    const esSend = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    await React.act(async () => {
      setTextarea(esText, "Escalation check");
      await flush();
    });
    await React.act(async () => {
      trustedClick(esSend);
      await flush();
    });
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "confirming" &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") !== "true" &&
        esSend.disabled,
      "escalate scenario keeps data-locked off while disabling duplicate send during Confirming send",
    );
    await React.act(async () => {
      await new Promise((r) => setTimeout(r, 2_200));
      await flush();
    });
    const checkBtn = Array.from(container.querySelectorAll("button")).find((b) => b.textContent === "Check status");
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "uncertain" &&
        container.textContent?.includes("Send not confirmed") &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") === "true" &&
        !!checkBtn,
      "exhausted auto-poll escalates to Send not confirmed with lock and Check status button",
    );
    statusOutcome = "sent";
    await React.act(async () => {
      checkBtn!.click();
      await flush();
    });
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "sent" &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") !== "true",
      "manual Check status unlocks composer once resolved",
    );

    // 7. Accepted outcome keeps composer unlocked with quiet pending note
    confirmOutcome = "accepted";
    const acceptedSelection: WhatsAppReplySelection = {
      ...selection,
      receiptScope: "scope-accepted",
    };
    await React.act(async () => {
      root.render(React.createElement(LocaleProvider, null, React.createElement(WhatsAppReplyComposer, { key: "accepted", selection: acceptedSelection })));
      await flush();
    });
    const acText = container.querySelector("textarea") as HTMLTextAreaElement;
    const acSend = container.querySelector("button.wa-reply-send") as HTMLButtonElement;
    await React.act(async () => {
      setTextarea(acText, "Accepted test");
      await flush();
    });
    await React.act(async () => {
      trustedClick(acSend);
      await flush();
    });
    assert(
      container.querySelector(".wa-reply-status")?.getAttribute("data-tone") === "accepted" &&
        container.textContent?.includes("Delivery pending") &&
        container.querySelector(".wa-reply-card")?.getAttribute("data-locked") !== "true" &&
        !acText.readOnly,
      "accepted outcome keeps composer unlocked with non-alarming pending status",
    );
  } finally {
    React.act(() => { root.unmount(); });
    container.remove();
    URL.createObjectURL = realCreateObjectURL;
    URL.revokeObjectURL = realRevokeObjectURL;
  }
}

async function main(): Promise<void> {
  await testProtocolAndChannel();
  await testScreenServicesMedia();
  await testComposerInteractions();
  console.log("Linked WhatsApp media UI host tests passed");
}

void main();
