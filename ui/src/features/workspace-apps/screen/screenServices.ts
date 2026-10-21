import { ApiError } from "../../../lib/api";
import { sessionHeaders } from "../../../lib/sessionKey";
import { displayEpoch, displayPlanKey, invalidateDisplay, observeRead, purgeDisplayCache } from "./screenDisplayCache";
import { RECORD_CALL_VERBS, RECORD_SLOT_VERBS, type ActionArgs, type ActionRefusal, type ActionResult, type RecordCallVerb, type RecordSlotVerb } from "./screenProtocol";
import type { MountReceipt } from "./screenLifecycle";
import type { RecordCard, SlotPlan } from "./screenSlot";

export const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
export function exact(v: Record<string, unknown>, keys: readonly string[]): boolean { return Object.keys(v).every(k => keys.includes(k)); }
const TOKEN = /^[a-f0-9]{64}$/;
const WRITES = new Set(["records.create", "records.update", "segments.save", "exclusions.save", "suppressions.add", "audience.prepare", "content.save", "content.clone", "proposals.apply", "proposals.discard", "sender-bindings.save", "whatsapp.draft.discard"]);
const SECRET_FIELDS = new Set(["action_token", "confirmation_token", "confirmation_nonce", "confirm_token", "csv_confirm_token", "scope_proof", "password", "credentials", "credential", "secret", "session_key", "host_command"]);
/** Service-only DTO projection. Private CSV delegation stays in this closure. */
function sanitized(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sanitized);
  if (!object(value)) return value;
  return Object.fromEntries(Object.entries(value).filter(([k]) => !SECRET_FIELDS.has(k)).map(([k, v]) => [k, sanitized(v)]));
}
function refusal(error: unknown, operation?: RecordCallVerb): ActionRefusal {
  const status = error instanceof ApiError ? error.status : 0;
  if (operation === "smtp.show" && error instanceof ApiError && status === 400 && (error.code === "smtp_sender_unbound" || error.code === "smtp_sender_revoked")) {
    return { code: error.code, text: "No live SMTP sender is bound. Configure a sender before sending." };
  }
  // CAD-1634: a WhatsApp refusal keeps the host's typed reason (for example
  // the door's `test_limit`), so the screen can name it.
  if (operation?.startsWith("whatsapp.") && error instanceof ApiError && status === 400 && error.code && /^[A-Za-z0-9_-]{1,64}$/.test(error.code)) {
    return { code: error.code, text: "Cadence refused this request. Inspect fresh status before retrying an effect." };
  }
  // Apart from explicit sender-state codes on smtp.show, the child consumes
  // canonical classes (a revision_conflict must still reach the editor as conflict).
  // CAD-1656: the confirm-time recompute refusal keeps its own code.
  if (status === 409 && error instanceof ApiError && error.code === "facts_changed") return { code: "facts_changed", text: "That changed. Refresh before trying again." };
  const code = ({ 401: "unauthorized", 403: "forbidden", 404: "not_found", 409: "conflict", 503: "unavailable" } as Record<number, string>)[status] ?? "failed";
  return { code, text: status === 401 || status === 403 ? "This app's access is no longer available." : status === 409 ? "That changed. Refresh before trying again." : "Cadence refused this request. Inspect fresh status before retrying an effect." };
}
export type HostCommand = { operation: string; args: ActionArgs; conversation_id?: string | null };
export interface RecordHost { current(): boolean; command(command: HostCommand): void; refused(): void }

/** One admitted mount; no caller-selected native route or RPC. */
export function recordServices(receipt: MountReceipt, domain: string, host: RecordHost) {
  const csv = new Map<string, { token: string; expires: number }>();
  let retired = false;
  let previewRevision = 0;
  let latestShow: { id: string; revision: number } | null = null;
  const current = () => !retired && host.current();
  const permitted = (op: string) => current() && receipt.host_contract === "screen-records.v1" && receipt.operations?.includes(op) && receipt.action_token;
  async function post(path: "call" | "slot-prepare" | "slot-confirm", body: ActionArgs): Promise<unknown> {
    if (!current()) throw new ApiError("Mount retired", 403);
    const response = await fetch(`/api/app-screen-services/${path}`, {
      method: "POST", credentials: "same-origin", cache: "no-store",
      headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
      body: JSON.stringify({ action_token: receipt.action_token, ...body }),
    });
    if (!current()) throw new ApiError("Mount retired", 403);
    const text = await response.text();
    if (!current()) throw new ApiError("Mount retired", 403);
    if (new TextEncoder().encode(text).byteLength > 1048576) throw new ApiError("Reply exceeds budget", 502);
    let value: unknown;
    try { value = JSON.parse(text); } catch { throw new ApiError("Invalid service reply", 502); }
    if (!response.ok) throw new ApiError("Service refused", response.status, object(value) && typeof value.code === "string" ? { code: value.code } : undefined);
    return value;
  }
  function failed(error: unknown, operation?: RecordCallVerb): ActionRefusal {
    csv.clear();
    if (current() && error instanceof ApiError && [401, 403].includes(error.status)) { retired = true; purgeDisplayCache(); host.refused(); }
    return refusal(error, operation);
  }
  function deliver(raw: unknown, executeCommand = true): unknown {
    if (!current()) throw new ApiError("Mount retired", 403);
    if (object(raw) && raw.host_command !== undefined) {
      const c = raw.host_command;
      if (!object(c) || !exact(c, ["operation", "args", "conversation_id"]) || typeof c.operation !== "string" || !object(c.args) || (c.conversation_id !== undefined && c.conversation_id !== null && (typeof c.conversation_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(c.conversation_id)))) throw new ApiError("Invalid host command", 502);
      if (executeCommand) host.command(c as unknown as HostCommand);
    }
    return sanitized(raw);
  }
  return {
    retire() { retired = true; csv.clear(); },
    async call(operation: RecordCallVerb, args: ActionArgs): Promise<ActionResult> {
      if (!permitted(operation) || !(RECORD_CALL_VERBS as readonly string[]).includes(operation)) return { ok: false, refusal: { code: "forbidden", text: "This operation is unavailable." } };
      const epoch = displayEpoch(domain);
      const previewId = (operation === "screen.preview.show" || operation === "screen.preview.clear") && typeof args.preview_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(args.preview_id) ? args.preview_id : null;
      let revision = previewRevision;
      if (previewId && operation === "screen.preview.show") {
        revision = ++previewRevision;
        latestShow = { id: previewId, revision };
      } else if (previewId && operation === "screen.preview.clear" && latestShow?.id === previewId) latestShow = null;
      try {
        const raw = await post("call", { operation, args });
        const stalePreview = !!previewId && ((operation === "screen.preview.show" && (latestShow?.id !== previewId || latestShow.revision !== revision)) || (operation === "screen.preview.clear" && latestShow?.id === previewId && latestShow.revision > revision));
        const data = deliver(raw, !stalePreview);
        if (WRITES.has(operation)) invalidateDisplay(domain);
        else if (receipt.display_scope_key) observeRead(receipt.display_scope_key, receipt.display_session_key!, domain, operation, args, data, epoch);
        return { ok: true, data };
      } catch (error) {
        const refusal = failed(error, operation);
        if (current() && previewId && operation === "screen.preview.show" && latestShow?.id === previewId && latestShow.revision === revision) host.command({ operation: "screen.preview.clear", args: { preview_id: previewId } });
        return { ok: false, refusal };
      }
    },
    async planner(operation: RecordSlotVerb, args: ActionArgs): Promise<SlotPlan | ActionRefusal> {
      if (!permitted(operation) || !(RECORD_SLOT_VERBS as readonly string[]).includes(operation)) return { code: "forbidden", text: "This operation is unavailable." };
      try {
        const raw = await post("slot-prepare", { operation, args });
        if (!object(raw) || !exact(raw, ["confirmation_token", "card"]) || typeof raw.confirmation_token !== "string" || !TOKEN.test(raw.confirmation_token) || !object(raw.card) || !exact(raw.card, operation === "whatsapp.broadcast.stop" || operation === "whatsapp.handoff.release" ? ["operation", "facts", "live"] : ["operation", "facts"]) || raw.card.operation !== operation || !object(raw.card.facts)) throw new ApiError("Invalid confirmation card", 502);
        const facts = raw.card.facts;
        const card = recordCard(operation, facts, object(raw.card.live) ? raw.card.live : undefined);
        let used = false;
        const expires = Date.now() + 60000;
        return { label: "Confirm", records: true, recordCard: card, run: async input => {
          if (used || !current() || Date.now() >= expires) return { ok: false, refusal: { code: "expired", text: "This confirmation has expired." } };
          used = true; // Never retry a confirmation, including an uncertain network outcome.
          try {
            const confirmation: ActionArgs = { confirmed: true };
            if (card.input) {
              const field = card.input.field;
              if (field === "brief" && input !== card.input.value) throw new ApiError("Brief changed; request a new card", 400);
              if (card.input.choices && !card.input.choices.some(c => c.value === input)) throw new ApiError("Select a current host choice", 400);
              confirmation[field] = field === "unsubscribe_origin" && !input?.trim() ? null : input ?? "";
            }
            if (operation === "records.csv.import") {
              const key = displayPlanKey(args), held = csv.get(key);
              csv.delete(key);
              if (!held || Date.now() >= held.expires) throw new ApiError("Confirm this exact CSV plan first", 409);
              confirmation.csv_confirm_token = held.token;
            }
            const result = await post("slot-confirm", { confirmation_token: raw.confirmation_token, confirmation });
            if (operation === "records.csv.confirm") {
              if (!object(result) || typeof result.confirm_token !== "string" || !/^confirm-[a-f0-9]{32}$/.test(result.confirm_token)) throw new ApiError("Missing private CSV receipt", 502);
              for (const [key, held] of csv) if (Date.now() >= held.expires) csv.delete(key);
              if (csv.size >= 4) csv.clear();
              csv.set(displayPlanKey(args), { token: result.confirm_token, expires: Date.now() + 60000 });
            }
            const data = deliver(operation === "records.csv.confirm" && object(result)
              ? { ...result, confirmation_required: result.state === "open" } : result);
            if (!["smtp.manage", "chat.campaign.open", "chat.campaign.brief", "chat.whatsapp.brief", "records.csv.confirm"].includes(operation)) invalidateDisplay(domain);
            return { ok: true, data };
          } catch (error) { return { ok: false, refusal: failed(error) }; }
        } };
      } catch (error) { return failed(error); }
    },
  };
}

function recordCard(operation: RecordSlotVerb, facts: ActionArgs, live?: ActionArgs): RecordCard {
  const rows: RecordCard["facts"] = [];
  const show = (label: string, value: unknown) => { if (value !== undefined && value !== null) rows.push({ label, value: typeof value === "object" ? JSON.stringify(value) : String(value) }); };
  const contentOuter = object(facts.content) ? facts.content : facts;
  const content = object(contentOuter.content) ? contentOuter.content : contentOuter;
  for (const [label, key] of [["Campaign", "campaign_id"], ["Subject", "subject"], ["Revision", "revision"], ["Name", "name"]]) show(label, content[key]);
  show("Send", facts.send_id); show("Customer", facts.customer_id); show("Recipient", facts.email); show("State", facts.state);
  show("Content revision", facts.content_revision); show("Content digest", facts.content_digest); show("CSV rows", facts.row_count); show("CSV plan", facts.summary);
  const audience = object(facts.audience) ? facts.audience : null;
  const freeze = audience && object(audience.freeze) ? audience.freeze : null;
  if (freeze) { show("Audience", freeze.freeze_id); show("Recipients", freeze.final_count); show("Recipient ceiling", freeze.max_recipients); show("Audience digest", freeze.digest); }
  if (operation === "sends.approve" && (!freeze || !Number.isSafeInteger(freeze.final_count) || typeof facts.content_revision !== "number" || typeof facts.send_digest !== "string")) throw new ApiError("Incomplete frozen send facts", 502);
  show("Frozen send digest", facts.send_digest);
  const outer = object(facts.binding) ? facts.binding : null;
  const binding = outer && object(outer.binding) ? outer.binding : outer;
  if (binding) for (const [label, key] of [["SMTP connection", "connection_id"], ["Binding revision", "link_revision"], ["Binding state", "state"], ["Binding digest", "digest"]]) show(label, binding[key]);
  if (operation === "whatsapp.reply.send") {
    // CAD-1562: recipient, window and text come from the daemon's
    // host-computed card for the prepared digest, never from the frame.
    const recipient = object(facts.recipient) ? facts.recipient : null;
    if (!recipient || typeof facts.text !== "string" || typeof facts.digest !== "string" || typeof facts.conversation_id !== "string") throw new ApiError("Incomplete reply facts", 502);
    show("To", [recipient.name, recipient.phone].filter(v => typeof v === "string" && v).join(" · ") || facts.conversation_id);
    show("Conversation", facts.conversation_id); show("Message", facts.text); show("Reply to message", facts.reply_to_message_id);
    show("Free-text window open until", facts.freeform_until); show("Reply digest", facts.digest);
  }
  if (operation === "whatsapp.template.send") {
    // CAD-1590: recipient from the live conversation; template, revision
    // and rendered body from the live approved template under the host digest.
    const recipient = object(facts.recipient) ? facts.recipient : null;
    const template = object(facts.template) ? facts.template : null;
    if (!recipient || !template || typeof facts.rendered_body !== "string" || typeof facts.digest !== "string") throw new ApiError("Incomplete template facts", 502);
    show("To", [recipient.name, recipient.phone].filter(v => typeof v === "string" && v).join(" · ") || facts.conversation_id);
    show("Number", facts.connection_id); show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision);
    show("Variables", facts.variables); show("Message", facts.rendered_body);
    show("Reply window", facts.reply_mode === "freeform" ? "Open" : "Closed — approved templates may still be sent");
    show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.test.send") {
    const template = object(facts.template) ? facts.template : null;
    if (typeof facts.to !== "string" || typeof facts.digest !== "string") throw new ApiError("Incomplete test facts", 502);
    show("Test number", facts.to);
    if (template) { show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision); }
    show("Sample message", facts.rendered_sample); show("Limit", facts.limit_note); show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.send") {
    // CAD-1590: the snapshot FavCRM froze at review; nothing from the frame.
    const snap = object(facts.snapshot) ? facts.snapshot : null;
    const template = snap && object(snap.template) ? snap.template : null;
    const audience = snap && object(snap.audience) ? snap.audience : null;
    if (!snap || !template || !audience || !Number.isSafeInteger(facts.eligible) || !Number.isSafeInteger(facts.excluded) || typeof facts.digest !== "string") throw new ApiError("Incomplete broadcast facts", 502);
    const connection = object(snap.connection) ? snap.connection : {};
    show("Number", connection.connectionId);
    show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision);
    show("Variables", facts.body_parameters); show("Header media", facts.header_media_url);
    show("Audience", audience.source === "segment" ? `Segment ${audience.segmentId}` : `${audience.accounts} selected contacts`);
    show("Eligible recipients", facts.eligible); show("Excluded", facts.excluded); show("Exclusion breakdown", snap.counts);
    show("Send time", typeof facts.send_at === "string" ? facts.send_at : "Now, after a 60-second undo window");
    show("Opt-outs", "Rechecked at send; the recipient count can only fall, never grow.");
    show("Messaging limit", "Numbers above your Meta messaging limit fail and appear in results.");
    show("Billing", "Meta charges your WhatsApp Business Account per delivered marketing message.");
    show("Review expires", snap.expires_at); show("Snapshot digest", snap.snapshotDigest); show("Send digest", facts.digest);
  }
  if (operation === "whatsapp.broadcast.stop") {
    const template = object(facts.template) ? facts.template : null;
    const stats = live && object(live.delivery_stats) ? live.delivery_stats : null;
    if (template) { show("Template", `${template.name} (${template.language})`); show("Template revision", template.revision); }
    show("Scheduled for", facts.scheduled_at);
    if (live) { show("Status (approximate, as of this card)", live.status); show("Delivery so far (approximate, still changing)", stats); }
    show("Effect", "Stops unsent recipients only. Messages already sent cannot be recalled. A stopped broadcast cannot resume; inside the undo window it returns to a draft instead.");
  }
  if (operation === "works.review" || operation === "works.export") {
    // CAD-1662: the card draws the doc's current revision the host re-proved;
    // the reviewer or exporter confirms that exact revision, nothing else.
    show("Title", facts.title); show("Kind", facts.kind); show("Revision", facts.revision); show("Digest", facts.digest);
    if (operation === "works.review") show("Decision", "Approve this revision, or request changes.");
    if (operation === "works.export") show("Format", "Export receipt for this approved revision only.");
  }
  if (operation === "whatsapp.handoff.release") {
    // CAD-1651: the takeover time is the bound fact; the contact is live and unbound.
    const contact = live && object(live.contact) ? live.contact : null;
    if (contact) show("Contact", [contact.name, contact.phone].filter(v => typeof v === "string" && v).join(" · "));
    show("Conversation", facts.conversation_id); show("Taken over since", facts.handoff_at);
    show("Effect", "FavCRM's AI may reply automatically again on this conversation.");
  }
  if (typeof facts.state === "string" && facts.state === "pending" && operation.startsWith("whatsapp.")) show("Approval", "Awaiting the workspace owner's approval; tapping again after approval sends it once.");
  show("Scope", facts.scope); show("Unsubscribe origin", facts.unsubscribe_origin ?? (operation === "origin.manage" ? "Not configured" : undefined));
  const card: RecordCard = { operation, facts: rows };
  if (operation === "works.review") card.input = { field: "choice", label: "Your decision on this exact revision", choices: [{ value: "approve", label: "Approve this revision" }, { value: "changes", label: "Request changes" }] };
  if (operation === "works.export") card.input = { field: "choice", label: "Export format for this approved revision", choices: [{ value: "html", label: "HTML" }, { value: "markdown", label: "Markdown" }] };
  if (operation === "smtp.test-send") card.input = { field: "to_email", label: "Test recipient (one email address)" };
  if (operation === "sends.resolve") card.input = { field: "resolution", label: "Resolve uncertain outcome — this does not resend", choices: [{ value: "accepted", label: "Accepted by the server" }, { value: "failed", label: "Failed" }] };
  if (operation === "origin.manage") card.input = { field: "unsubscribe_origin", label: "Daemon-global unsubscribe origin (blank clears it)", value: typeof facts.unsubscribe_origin === "string" ? facts.unsubscribe_origin : "" };
  if (operation === "chat.campaign.brief") { show("Effect", facts.effect); card.input = { field: "brief", label: "Send this brief to the assistant", value: typeof facts.brief === "string" ? facts.brief : "" }; }
  if (operation === "chat.whatsapp.brief") {
    // CAD-1636: the host names the contact, the bound and the model; the guidance is shown read-only.
    if (typeof facts.bound !== "string" || typeof facts.model !== "string") throw new ApiError("Incomplete brief facts", 502);
    show("Contact", facts.contact ?? "Unknown contact"); show("What the assistant reads", facts.bound); show("Model", facts.model); show("Effect", facts.effect);
    card.input = { field: "brief", label: "Your guidance for the assistant", value: typeof facts.guidance === "string" ? facts.guidance : "" };
  }
  if (operation === "smtp.bind" || operation === "smtp.rebind") {
    const choices = Array.isArray(facts.connections) ? facts.connections.filter(object).filter(c => typeof c.connection_id === "string").map(c => ({ value: c.connection_id as string, label: `${c.connection_id} — ${JSON.stringify(c.sender)}` })) : [];
    card.input = { field: "connection_id", label: "Admitted SMTP connection", choices };
  }
  return card;
}
