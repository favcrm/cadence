import { recordServices } from "../src/features/workspace-apps/screen/screenServices";
import type { MountReceipt } from "../src/features/workspace-apps/screen/screenLifecycle";
import type { RecordCard } from "../src/features/workspace-apps/screen/screenSlot";

// CAD-2146: the send, content and test-send cards read plainly for both host
// fact shapes, and the prepare-reply validation is unchanged.
const check = (ok: unknown, why: string): void => { if (!ok) throw new Error(why); };
const savedFetch = globalThis.fetch;
const receipt: MountReceipt = {
  mount: `/api/app-screen/${"c".repeat(64)}`, bridge_nonce: "d".repeat(64), generation: 1,
  tag: "crm", action_token: "a".repeat(64), tools: [], host_contract: "screen-records.v1",
  operations: ["sends.approve", "content.approve", "smtp.test-send", "works.review"], navigation: null, install_id: "own-install", context_id: "own-context",
  digest: `sha256:${"e".repeat(64)}`, display_scope_key: "f".repeat(64), display_session_key: "1".repeat(64), remote_images: [],
};

async function card(operation: string, facts: Record<string, unknown>, args: Record<string, unknown> = {}): Promise<RecordCard | string> {
  globalThis.fetch = async () => new Response(JSON.stringify({ confirmation_token: "b".repeat(64), card: { operation, facts } }), { status: 200, headers: { "Content-Type": "application/json" } });
  const services = recordServices(receipt, "own-domain", { current: () => true, refused: () => {}, command: () => {} });
  const plan = await services.planner(operation as never, args);
  return "recordCard" in plan && plan.recordCard ? plan.recordCard : "refused";
}
const labels = (rows: { label: string }[] | undefined) => (rows ?? []).map(r => r.label).join("|");

async function main(): Promise<void> {
  const freeze = { freeze_id: "freeze-1", base: { mode: "segment", segment_id: "seg-9" }, final_count: 1204, max_recipients: 2000, digest: "sha256:aa" };
  // Hosted shape: subject present, binding without a digest, no sender.
  const hosted = await card("sends.approve", { send_id: "send-1", campaign_id: "camp-1", subject: "Hi {{first_name|Friend}}", state: "prepared", content_revision: 2, content_digest: "sha256:bb", send_digest: "sha256:cc", audience: { freeze }, binding: { connection_id: "conn-1", link_revision: 3, state: "live" } });
  check(typeof hosted !== "string", "hosted send card builds");
  if (typeof hosted !== "string") {
    check(hosted.title === "Send to 1,204 people?" && hosted.confirm === "Send to 1,204 people", "send title and button name the count");
    check(labels(hosted.facts) === "Email|Audience|Recipients", `hosted rows: ${labels(hosted.facts)}`);
    check(hosted.facts[0].value === "Hi {first name}", "merge tag shown without its fallback");
    check(hosted.facts[2].strong === true && hosted.facts[2].value === "1,204", "recipients stand out");
    check(hosted.note === "This can't be undone.", "irreversible note");
    check(labels(hosted.details).includes("Send digest") && labels(hosted.details).includes("Binding state"), "technical values stay in details");
  }
  // Native shape: no subject, binding nested with a digest, sender carried when the host has one.
  const native = await card("sends.approve", { send_id: "send-1", campaign_id: "camp-1", state: "prepared", content_revision: 2, content_digest: "sha256:bb", send_digest: "sha256:cc", audience: { freeze }, binding: { binding: { connection_id: "conn-1", link_revision: 3, state: "live", digest: "sha256:dd", sender: { name: "Shop", address: "shop@example.test" } } }, unsubscribe_origin: "https://x.example.test" });
  check(typeof native !== "string" && labels(native.facts) === "Audience|Recipients|From" && native.facts[2].value === "Shop <shop@example.test>", "native rows omit the subject and show the sender when carried");
  // Validation is unchanged: each required fact still refuses.
  for (const bad of [{ content_revision: undefined }, { send_digest: undefined }, { audience: {} }, { audience: { freeze: { ...freeze, final_count: "9" } } }]) {
    const r = await card("sends.approve", { send_id: "s", content_revision: 2, send_digest: "sha256:cc", audience: { freeze }, ...bad });
    check(r === "refused", "an incomplete frozen send is still refused");
  }
  const content = await card("content.approve", { content: { campaign_id: "camp-1", subject: "Spring", revision: 4, name: "Spring", content_digest: "sha256:ee" } });
  check(typeof content !== "string" && content.title === "Approve this email?" && labels(content.facts) === "Subject|Version" && content.details?.length === 3, "content card");
  const test = await card("smtp.test-send", { binding: { binding: { connection_id: "conn-1", link_revision: 3, state: "live", digest: "sha256:dd" } } });
  check(typeof test !== "string" && test.input?.field === "to_email" && test.facts.length === 0 && labels(test.details) === "SMTP connection|Binding revision|Binding state|Binding digest", "test-send card omits what the host did not send");
  // Any other operation keeps its card.
  const other = await card("works.review", { title: "Plan", revision: 3 }, { id: "doc-1", revision: 3 });
  check(typeof other !== "string" && !other.title && !other.details && labels(other.facts) === "Revision|Title|Revision|Decision", "other operations are untouched");
}

main().then(
  () => { globalThis.fetch = savedFetch; console.log("plainConfirmCards: ok"); },
  error => { globalThis.fetch = savedFetch; throw error; },
);
