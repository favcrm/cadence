/** DEMOCOMPANY-35: exercise the untrusted frame boundary, not a send service. */
import { ScreenChannel, type ScreenActions } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { ScreenPush } from "../src/features/workspace-apps/screen/screenProtocol";

function assert(value: unknown, message: string): asserts value {
  if (!value) throw new Error(message);
}

const nonce = "a".repeat(64);
const digest = `sha256:${"c".repeat(64)}`;
const capability = "screen-whatsapp-compose.v1";
const jid = "15550001111@s.whatsapp.net";

interface Effects {
  selected: (string | null)[];
  calls: number;
  plans: number;
  slots: number;
  failed: number;
  ready: number;
}

function mount(options: { tag?: string; negotiated?: boolean; readOnly?: boolean } = {}) {
  const tag = options.tag ?? "waba";
  const receipt = {
    mount: `/api/app-screen/${"b".repeat(64)}`, bridge_nonce: nonce, generation: 1, tag,
    action_token: "d".repeat(64), tools: [], host_contract: "screen-records.v1" as const,
    operations: options.readOnly ? ["whatsapp.conversations.show"] : ["whatsapp.conversations.show", "whatsapp.reply.prepare", "whatsapp.reply.send", "whatsapp.reply.status"],
    navigation: null, install_id: "synthetic-install", context_id: "synthetic-context",
    digest, display_scope_key: "e".repeat(64), display_session_key: "f".repeat(64), remote_images: [],
  };
  const projection: ScreenPush = {
    v: 2, op: "screen", install_id: receipt.install_id, digest, tag, context_id: receipt.context_id,
    installation: { install_id: receipt.install_id, name: "waba", title: "Synthetic", version: "1.0.0", digest, summary: "" },
    contexts: [{ id: receipt.context_id, label: "Synthetic" }], runs: [], outbox: [], now: 1,
    publish_intents: { status: "ok", withheld: 0, rows: [] }, actions: receipt.operations,
  };
  const effects: Effects = { selected: [], calls: 0, plans: 0, slots: 0, failed: 0, ready: 0 };
  let changed = Promise.withResolvers<void>();
  const notify = () => { const previous = changed; changed = Promise.withResolvers<void>(); previous.resolve(); };
  const until = async (predicate: () => boolean) => { while (!predicate()) await changed.promise; };
  const actions = {
    call: async () => { effects.calls++; return { ok: true, data: null }; },
    planner: () => { effects.plans++; return null; },
    onSlot: () => { effects.slots++; },
    onLink: () => { throw new Error("unexpected link"); },
    onWhatsAppCompose: (conversationId: string | null) => { effects.selected.push(conversationId); notify(); },
  } as unknown as ScreenActions;
  const source = {} as Window;
  const channel = new ScreenChannel(source, receipt, projection, () => {}, () => { effects.failed++; notify(); }, () => { effects.ready++; notify(); }, undefined, actions);
  const pair = new MessageChannel();
  const messages: Record<string, unknown>[] = [];
  pair.port2.onmessage = event => { messages.push(event.data as Record<string, unknown>); notify(); };
  pair.port2.start();
  channel.receive({ source, origin: "null", data: { v: 1, op: "init", tag, bridge_nonce: nonce, generation: 1 }, ports: [pair.port1] });
  pair.port2.postMessage({ v: 1, op: "ready", accepts: ["screen.v2", "screen-records.v1", ...(options.negotiated === false ? [] : [capability])] });
  return {
    channel, effects, messages, until,
    send: (message: unknown) => pair.port2.postMessage(message),
    close: () => { channel.close(); pair.port2.close(); },
  };
}

function noAuthority(effects: Effects, label: string): void {
  assert(effects.calls === 0 && effects.plans === 0 && effects.slots === 0, `${label}: no action call, planner or confirmation slot may run`);
}

async function main(): Promise<void> {
  const live = mount();
  try {
    await live.until(() => live.effects.failed === 1 || (live.effects.ready === 1 && live.messages.some(m => m.op === "screen")));
    assert(live.effects.failed === 0, "negotiated records frame must become ready");
    live.send({ v: 2, op: "whatsapp-compose", conversation_id: jid });
    await live.until(() => live.effects.failed === 1 || live.effects.selected.length === 1);
    assert(live.effects.failed === 0, "valid selection must not close the frame");
    live.send({ v: 2, op: "whatsapp-compose", conversation_id: null });
    await live.until(() => live.effects.failed === 1 || live.effects.selected.length === 2);
    assert(live.effects.failed === 0, "valid clear must not close the frame");
    assert(JSON.stringify(live.effects.selected) === JSON.stringify([jid, null]), "only the nominated thread and clear reach the host");
    live.channel.whatsappComposeState(true, false);
    live.channel.whatsappComposeState(false, true);
    await live.until(() => live.messages.filter(m => m.op === "whatsapp-compose-state").length === 2);
    assert(JSON.stringify(live.messages.filter(m => m.op === "whatsapp-compose-state")) === JSON.stringify([
      { v: 2, op: "whatsapp-compose-state", dirty: true, refresh: false },
      { v: 2, op: "whatsapp-compose-state", dirty: false, refresh: true },
    ]), "state carries only dirty and refresh hints");
    noAuthority(live.effects, "selection and state");
  } finally { live.close(); }

  const forgeries: Record<string, unknown>[] = [
    { draft: "forged text" }, { text: "forged text" }, { submit: true }, { confirmation: "forged" },
    { approved: true }, { token: "forged" }, { recipient: jid }, { label: "forged" },
    { command: "send" }, { conversation_id: "not-a-linked-jid" },
  ];
  for (const fields of forgeries) {
    const frame = mount();
    try {
      await frame.until(() => frame.effects.ready === 1 || frame.effects.failed === 1);
      assert(frame.effects.ready === 1, "negotiated forgery frame must become ready");
      frame.send({ v: 2, op: "whatsapp-compose", conversation_id: jid, ...fields });
      frame.send({ v: 2, op: "whatsapp-compose", conversation_id: jid });
      await frame.until(() => frame.effects.failed === 1 || frame.effects.selected.length > 0);
      assert(frame.effects.failed === 1 && frame.effects.selected.length === 0, `malformed selector must close without selecting: ${JSON.stringify(fields)}`);
      noAuthority(frame.effects, "forged selector");
    } finally { frame.close(); }
  }

  for (const [label, options] of [
    ["unnegotiated", { negotiated: false }],
    ["non-WABA", { tag: "main" }],
    ["read-only", { readOnly: true }],
  ] as const) {
    const frame = mount(options);
    try {
      await frame.until(() => frame.effects.ready === 1 || frame.effects.failed === 1);
      if (frame.effects.ready) {
        frame.send({ v: 2, op: "whatsapp-compose", conversation_id: jid });
        await frame.until(() => frame.effects.failed === 1 || frame.effects.selected.length > 0);
        assert(frame.effects.failed === 1, `${label} selector must close`);
      }
      assert(frame.effects.selected.length === 0, `${label} cannot nominate a thread`);
      noAuthority(frame.effects, label);
    } finally { frame.close(); }
  }
  console.log("DEMOCOMPANY-35 independent WhatsApp compose boundary acceptance passed");
}

void main();
