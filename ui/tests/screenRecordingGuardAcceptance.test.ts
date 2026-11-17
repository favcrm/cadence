import { parseMount, ScreenChannel, type MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { AudioRecordingRequest, ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";

function check(value: unknown, label: string): void {
  if (!value) throw new Error(label);
}

class Port {
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  start(): void {}
  close(): void { this.closed = true; }
  postMessage(value: unknown): void { this.sent.push(value); }
  receive(value: unknown): void { this.onmessage?.({ data: value }); }
}

const token = "a".repeat(64);
const digest = `sha256:${"d".repeat(64)}`;
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"b".repeat(64)}`,
  bridge_nonce: "c".repeat(64),
  generation: 1,
  tag: "app",
  action_token: token,
  tools: [],
  host_contract: "screen-records.v1",
  operations: ["records.show", "records.update"],
  navigation: null,
  install_id: "inst-1",
  context_id: "ctx-1",
  digest,
  display_scope_key: "e".repeat(64),
  display_session_key: "f".repeat(64),
  remote_images: [],
};
const projection: ScreenPush = {
  v: 1,
  op: "screen",
  install_id: "inst-1",
  digest,
  tag: "app",
  context_id: "ctx-1",
  installation: { install_id: "inst-1", digest, title: "App", name: "app", version: "1", summary: "" },
  contexts: [{ id: "ctx-1", label: "context" }],
  runs: [],
  outbox: [],
  now: 1,
  publish_intents: { status: "loading", withheld: 0, rows: [] },
};

async function run(): Promise<void> {
  check(parseMount(receipt, "app"), "the records receipt is valid");
  const dispatched: { request: AudioRecordingRequest; actionToken: string }[] = [];
  const host = {
    call: async () => ({ ok: true as const, data: null }),
    planner: async () => ({ code: "forbidden", text: "not a slot" }),
    onSlot: () => {},
    onLink: () => {},
    audioSession: async () => ({ ok: false as const, refusal: { code: "unavailable", text: "not used" } }),
    audioRecording: async (request: AudioRecordingRequest, opts: { actionToken: string }) => {
      dispatched.push({ request, actionToken: opts.actionToken });
      return { ok: true as const, data: { ok: true } };
    },
  };
  const mount = (withAudio: boolean) => {
    const source = {} as Window;
    const port = new Port();
    const channel = new ScreenChannel(source, receipt, projection, () => {}, () => {}, () => {}, async () => null, host);
    channel.receive({
      source,
      origin: "null",
      data: { v: 1, op: "init", tag: "app", bridge_nonce: receipt.bridge_nonce, generation: 1 },
      ports: [port as unknown as MessagePort],
    });
    port.receive({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1", ...(withAudio ? ["screen-audio.v2"] : [])] });
    check(!port.closed, "records screen reached ready");
    return port;
  };
  const request = { v: 2, op: "audio-recording", id: "r1", action: "delete", record_id: "meeting-1" };
  const missingCapability = mount(false);
  missingCapability.receive(request);
  check(missingCapability.closed, "a screen without audio v2 is closed when it requests recording storage");
  check(dispatched.length === 0, "missing audio v2 refuses before the storage dispatcher");

  const admitted = mount(true);
  admitted.receive(request);
  check(dispatched.length === 1, "the same request dispatches after audio v2 is negotiated");
  check(dispatched[0]?.actionToken === token && dispatched[0]?.request.action === "delete", "the host passes its own receipt token and requested operation");
  await Promise.resolve();
  await Promise.resolve();
  const reply = admitted.sent.find(message => (message as { id?: string }).id === "r1") as { op?: string; ok?: boolean } | undefined;
  check(reply?.op === "audio-recording-reply" && reply.ok === true, "the admitted request receives a successful recording reply");
  check(!admitted.closed, "the admitted channel remains open");
}

void run();
