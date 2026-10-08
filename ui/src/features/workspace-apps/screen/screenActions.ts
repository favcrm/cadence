import { ApiError } from "../../../lib/api";
import { rememberContext } from "../contextSelection";
import { workspaceApps, type AppBinding, type AppContext, type CapabilityQuote, type Connection, type Installation, type WorkspaceRun } from "../workspaceApps";
import { socialPublish, type PreparedOwnerStatus, type PublishIntent } from "../socialPublish";
import { contextDefaultKeys, instagramLink, isZone } from "./screenProjection";
import type { ActionRefusal, ActionResult, CallVerb, SlotVerb } from "./screenProtocol";
import { SLOT_TTL_MS, type Planner, type SlotPlan } from "./screenSlot";

/**
 * CAD-1123 HP3 — what the host does for a frame's verbs. Every verb is a
 * generic host primitive: it works from the installed bundle's own
 * declarations and the board's operator session, and names no app. The
 * daemon re-proves the operator and every bound on each write, so a verb
 * here is a convenience of the board, never the authority.
 */
export interface ActionContext {
  installation: Installation;
  installId: string;
  contextId: string;
  runs: WorkspaceRun[];
  /** Board-held reads the publish planners validate against (never invented):
   *  the install's bindings, its connections and the scope-tagged intent read. */
  bindings: AppBinding[];
  connections: Connection[];
  intents: PublishIntent[];
  /** Reload the board's data after a write. */
  onChanged: () => void;
}

const NO: ActionRefusal = { code: "denied", text: "Only the operator can do that." };
const refuse = (code: string, text: string): ActionResult => ({ ok: false, refusal: { code, text } });
const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const exact = (value: Record<string, unknown>, keys: string[]) => Object.keys(value).sort().join() === [...keys].sort().join();

/** A daemon refusal, as a plain line. The daemon's wording never reaches the frame. */
export function plainRefusal(error: unknown): ActionRefusal {
  if (!(error instanceof ApiError)) return { code: "failed", text: "That didn't work. Nothing was started." };
  if (error.status === 401 || error.status === 403) return NO;
  const text = error.message.toLowerCase();
  if (text.includes("canonical host mapping") ||
      (text.includes("canonical") && text.includes("cadencecloud.app")))
    return { code: "not_configured", text: "Owner portal not configured: the board host must map one valid AOS workspace slug under cadencecloud.app. Nothing was prepared." };
  if (text.includes("company_slug"))
    return { code: "not_configured", text: "Owner portal not configured: the board's authoritative company mapping is missing. Nothing was prepared." };
  if (text.includes("authorize_url"))
    return { code: "not_configured", text: "Owner portal not configured: set AGENTICOS_BOARD_AUTHORIZE_URL to the app origin. Nothing was prepared." };
  if (text.includes("not configured"))
    return { code: "not_configured", text: "The owner portal isn't configured for this board. Nothing was prepared." };
  if (text.includes("price_changed")) return { code: "price_changed", text: "Something changed. Please try again." };
  if (text.includes("no default team")) return { code: "team_missing", text: "Set this app's team in Settings first." };
  if (text.includes("worker")) return { code: "team_unavailable", text: "A team member is unavailable right now." };
  if (text.includes("stale")) return { code: "stale", text: "That changed somewhere else. Reload and try again." };
  if (error.status === 409) return { code: "refused", text: "Cadence couldn't do that. Nothing was started." };
  return { code: "failed", text: "That didn't work. Nothing was started." };
}

const randomRequest = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("");
const pendingOwnerRequests = new Map<string, string>();
function ownerRequestFor(scope: string): string | null {
  const existing = pendingOwnerRequests.get(scope);
  if (existing) return existing;
  // Never evict an uncertain request: losing its key could turn a retry into
  // a second prepared intent. Refuse new scopes when the bounded cache is full.
  if (pendingOwnerRequests.size >= 64) return null;
  const request = randomRequest();
  pendingOwnerRequests.set(scope, request);
  return request;
}

async function readRun(ctx: ActionContext, args: Record<string, unknown>): Promise<ActionResult> {
  if (!exact(args, ["run_id"]) || typeof args.run_id !== "string") return refuse("bad_args", "That request is not valid.");
  const run = ctx.runs.find(value => value.id === args.run_id && value.install_id === ctx.installId &&
    (!ctx.contextId || value.context_id === ctx.contextId));
  if (!run) return refuse("not_found", "That item is not available.");
  const approve = run.reviews.find(review => review.decision === "approve");
  const last = run.reviews[run.reviews.length - 1];
  const artifact = approve && run.artifacts.find(value => value.digest === approve.artifact_digest && value.media_type.startsWith("text/"));
  let caption: string | undefined;
  if (artifact) {
    const text = await workspaceApps.artifact(artifact.id);
    if (text.id === artifact.id && typeof text.text === "string") caption = text.text.slice(0, 8192);
  }
  return { ok: true, data: { run_id: run.id, ...(caption !== undefined ? { caption } : {}),
    ...(last ? { review: { decision: last.decision.slice(0, 32), rationale: last.rationale.slice(0, 2000) } } : {}) } };
}

const DEFAULT_LABEL = "Default";
/** Save the content defaults into the installation's "Default" context,
 *  creating it when there is none. `expected_revision` is the revision the
 *  frame last saw (0 when no Default context existed): a stale one is refused
 *  and nothing is written (the daemon holds the same compare-and-swap). */
async function saveDefaults(ctx: ActionContext, args: Record<string, unknown>): Promise<ActionResult> {
  if (!exact(args, ["values", "expected_revision"]) || !isObject(args.values) ||
      !Number.isSafeInteger(args.expected_revision) || (args.expected_revision as number) < 0)
    return refuse("bad_args", "That request is not valid.");
  const allowed = contextDefaultKeys(ctx.installation);
  const values: Record<string, string> = {};
  for (const [key, value] of Object.entries(args.values)) {
    if (!allowed.has(key) || typeof value !== "string" || value.length > 512) return refuse("bad_args", "That setting can't be saved.");
    values[key] = value;
  }
  if (Object.keys(values).length === 0) return refuse("bad_args", "There is nothing to save.");
  const expected = args.expected_revision as number;
  const contexts = await workspaceApps.contexts(ctx.installId);
  const mine = (value: AppContext) => value.install_id === ctx.installId && value.config.label === DEFAULT_LABEL;
  const current = contexts.find(value => mine(value) && value.state === "active");
  let saved: AppContext;
  if (!current) {
    if (expected !== 0) return refuse("stale", "That changed somewhere else. Reload and try again.");
    // One request id per (installation, archived Default count): a repeat
    // returns the same context, a second concurrent save with other values is refused.
    const archived = contexts.filter(value => mine(value) && value.state !== "active").length;
    saved = await workspaceApps.createContext(ctx.installId, { label: DEFAULT_LABEL, input_defaults: values,
      request_id: `default-context-${ctx.installId}-${archived}` });
  } else {
    if (current.revision !== expected) return refuse("stale", "That changed somewhere else. Reload and try again.");
    saved = await workspaceApps.updateContext(ctx.installId, current.id, { expected_revision: current.revision,
      label: DEFAULT_LABEL, input_defaults: { ...current.config.input_defaults, ...values } });
  }
  // The defaults live in this context: make it the board's selection so the
  // frame (and the runs it starts) read and use them.
  rememberContext(ctx.installId, saved.id);
  ctx.onChanged();
  return { ok: true, data: { context_id: saved.id, revision: saved.revision } };
}

/** `open-link`: https on allowlisted hosts only. The host shows a link the
 *  operator clicks (the SafeLink policy); the frame cannot navigate anything. */
function openLink(args: Record<string, unknown>, showLink: (url: string) => void): ActionResult {
  if (!exact(args, ["url"]) || typeof args.url !== "string") return refuse("bad_args", "That link is not valid.");
  const url = instagramLink(args.url);
  if (!url) return refuse("link_blocked", "That link can't be opened here.");
  showLink(url);
  return { ok: true, data: {} };
}

/** The direct (non-spending) verbs. The set is closed in `screenProtocol`. */
export function runCall(ctx: ActionContext, verb: CallVerb, args: Record<string, unknown>,
  ui: { showLink(url: string): void }): Promise<ActionResult> {
  const table: Record<CallVerb, () => Promise<ActionResult> | ActionResult> = {
    "read.run": () => readRun(ctx, args),
    "context.defaults.save": () => saveDefaults(ctx, args),
    "publish.accounts.refresh": () => refreshPublishAccounts(ctx, args),
    "publish.settings.save": () => savePublishSettings(ctx, args),
    "open-link": () => openLink(args, ui.showLink),
  };
  return Promise.resolve().then(table[verb]).catch((error: unknown): ActionResult => ({ ok: false, refusal: plainRefusal(error) }));
}

const LABEL_MAX = 40;
const INPUT_MAX = 16;
const VALUE_MAX = 8192;
/** `run.start`: validate against the installed workflow, fetch the quotes the
 *  daemon will compare with the run's frozen ones, and name the work with the
 *  workflow's own label. No amount is shown or sent to the frame. */
async function planRunStart(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  const keys = Object.keys(args).sort().join();
  if (!["inputs,workflow", "inputs,selected_post_id,source_receipt_id,workflow", "carry,inputs,workflow"].includes(keys) ||
      typeof args.workflow !== "string" || !isObject(args.inputs)) return { code: "bad_args", text: "That request is not valid." };
  const flow = ctx.installation.workflows?.find(value => value.name === args.workflow);
  if (!flow) return { code: "unknown_workflow", text: "That isn't available." };
  const declared = new Set(flow.inputs.map(input => input.name));
  const entries = Object.entries(args.inputs);
  if (entries.length > INPUT_MAX || entries.some(([key, value]) => !declared.has(key) || typeof value !== "string" || value.length > VALUE_MAX))
    return { code: "bad_args", text: "That request is not valid." };
  const inputs = Object.fromEntries(entries as [string, string][]);
  const source = "source_receipt_id" in args
    ? (typeof args.source_receipt_id === "string" && typeof args.selected_post_id === "string"
      && args.source_receipt_id.length <= 128 && args.selected_post_id.length <= 128
      ? { source_receipt_id: args.source_receipt_id, selected_post_id: args.selected_post_id } : null)
    : {};
  if (!source) return { code: "bad_args", text: "That request is not valid." };
  // CAD-1143 Redo carry: exactly `{from_run_id, retain}` — source run and
  // untouched half only. Mixed source-post provenance is already refused by
  // the key sets above; the daemon derives everything else and re-proves it.
  // The host checks scope only (the source run must be visible here).
  let carry: { from_run_id: string; retain: "image" | "text" } | undefined;
  if ("carry" in args) {
    const asked = args.carry;
    if (!isObject(asked) || !exact(asked, ["from_run_id", "retain"]) ||
        typeof asked.from_run_id !== "string" || asked.from_run_id.length === 0 ||
        asked.from_run_id.length > 128 || (asked.retain !== "image" && asked.retain !== "text"))
      return { code: "bad_args", text: "That request is not valid." };
    const seen = ctx.runs.find(value => value.id === asked.from_run_id && value.install_id === ctx.installId &&
      (!ctx.contextId || (value.context_id ?? "") === ctx.contextId));
    if (!seen) return { code: "not_found", text: "That item is not available." };
    carry = { from_run_id: asked.from_run_id, retain: asked.retain };
  }
  const slots = flow.capability_slots ?? [];
  const expected: Record<string, CapabilityQuote["quote"]> = {};
  try {
    for (const slot of slots)
      expected[slot] = (await workspaceApps.bindingQuote(ctx.installId, slot, ctx.contextId || undefined)).quote;
  } catch (error) { return plainRefusal(error); }
  const label = (flow.label ?? "").replace(/\s+/g, " ").trim().slice(0, LABEL_MAX) || "Start";
  const request = randomRequest();
  const installId = ctx.installId;
  const contextId = ctx.contextId;
  return { label, run: async () => {
    try {
      await workspaceApps.startRun({ install_id: installId, workflow: flow.name, inputs, request_id: request,
        expected_quotes: expected, ...(contextId ? { context_id: contextId } : {}), ...source,
        ...(carry ? { carry } : {}) });
      ctx.onChanged();
      return { ok: true, data: {} };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

/** The spend/publish verbs. HP4 adds `publish.*` here; the set is closed in `screenProtocol`. */
export function makePlanner(get: () => ActionContext): Planner {
  return (verb: SlotVerb, args, signal) => {
    const table: Record<SlotVerb, () => Promise<SlotPlan | ActionRefusal>> = {
      "run.start": () => planRunStart(get(), args),
      "publish.start": () => planPublishStart(get(), args, signal),
      "publish.reschedule": () => planPublishReschedule(get(), args),
      "publish.cancel": () => planPublishCancel(get(), args),
      "publish.send_now": () => planPublishSendNow(get(), args),
    };
    return table[verb]();
  };
}

/** CAD-1143 — the shared shape grammar for the publish verbs. Ids are the
 *  daemon's segment shape; the destination is either an unambiguous raw id
 *  or a key returned by the fresh accounts action. The zone needs the daemon's
 *  charset AND a real IANA name (shared with the projection). Time is the
 *  daemon's authority — the host checks shape only, never futurity. */
const SEGMENT = /^[A-Za-z0-9._-]{1,128}$/;
const DESTINATION = /^[A-Za-z0-9._-]{1,120}$/;
const DESTINATION_SELECTOR = /^[A-Za-z0-9_-]{1,80}\|(instagram|facebook)\|[A-Za-z0-9._-]{1,120}$/;
const ZONE_CHARS = /^[A-Za-z0-9/_+-]{1,64}$/;
const isEpoch = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;

/** The live publication binding for this scope: configured, on this
 *  install's digest, in this context. Anything else means publishing is not
 *  set up here, and every publish verb is refused before any write. */
function publicationBinding(ctx: ActionContext): AppBinding | undefined {
  return ctx.bindings.find(value => value.install_id === ctx.installId &&
    (value.context_id ?? "") === ctx.contextId && value.slot === "publication" &&
    value.state === "configured" && value.config.bundle_digest === ctx.installation.digest);
}

/** One in-scope intent by id. An empty board context (`""`) lists the
 *  whole install, so any install-matching row counts; otherwise the row's
 *  own context must equal the board's. Callers always use the row's OWN
 *  scope for the write — the daemon refuses any other. */
function inScopeIntent(ctx: ActionContext, intentId: string): PublishIntent | undefined {
  return ctx.intents.find(intent => intent.intent_id === intentId && intent.install_id === ctx.installId &&
    (ctx.contextId === "" || (intent.context_id ?? "") === ctx.contextId));
}

const OWNER_REQUEST_TIMEOUT_MS = 10_000;
const PUBLISH_STATUS_UNCERTAIN: ActionRefusal = {
  code: "publish_status_uncertain",
  text: "Cadence couldn't confirm the publish status. Check the queue before trying again.",
};

function uncertainOwnerAttach(): ActionResult {
  return { ok: false, refusal: PUBLISH_STATUS_UNCERTAIN };
}

const OWNER_STATUS_POLL_MS = 1_000;
const OWNER_REFUSED: ActionRefusal = {
  code: "owner_action_refused",
  text: "The owner action is unavailable or was refused. Nothing was queued.",
};

async function ownerStatusWithTimeout(
  preparedId: string,
  scope: { installId: string; contextId: string | null },
  signal: AbortSignal,
): Promise<PreparedOwnerStatus> {
  const attempt = new AbortController();
  let timedOut = false;
  const abortWithSlot = () => attempt.abort();
  if (signal.aborted) abortWithSlot();
  else signal.addEventListener("abort", abortWithSlot, { once: true });
  const timeout = setTimeout(() => { timedOut = true; attempt.abort(); }, OWNER_REQUEST_TIMEOUT_MS);
  try {
    return await socialPublish.statusIntent({
      prepared_id: preparedId, install_id: scope.installId, context_id: scope.contextId,
    }, scope, attempt.signal);
  } catch (error) {
    if (timedOut) throw new Error("owner status request timed out");
    throw error;
  } finally {
    clearTimeout(timeout);
    signal.removeEventListener("abort", abortWithSlot);
  }
}

function delay(ms: number, signal: AbortSignal): Promise<boolean> {
  if (signal.aborted) return Promise.resolve(false);
  return new Promise(resolve => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const done = (value: boolean) => {
      if (timer !== undefined) clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      resolve(value);
    };
    const abort = () => done(false);
    timer = setTimeout(() => done(true), ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}

async function reportAttachedIntent(
  ctx: ActionContext,
  prepared: Awaited<ReturnType<typeof socialPublish.prepareIntent>>,
  intentId: string,
  signal: AbortSignal,
  requestScope: string,
): Promise<ActionResult> {
  try {
    const intent = (await socialPublish.show(intentId, signal)).intent;
    if (signal.aborted || intent.intent_id !== intentId ||
        intent.idempotency_key !== prepared.request || intent.install_id !== prepared.install_id ||
        intent.context_id !== prepared.context_id || intent.run_id !== prepared.run_id) {
      if (!signal.aborted) ctx.onChanged();
      return uncertainOwnerAttach();
    }
    if (intent.state === "queued" && (!intent.claim_armed || (intent.claim_after_epoch ?? 0) <= 0)) {
      if (!signal.aborted) ctx.onChanged();
      return uncertainOwnerAttach();
    }
    if (intent.state === "queued" && intent.claim_armed && (intent.claim_after_epoch ?? 0) > 0) {
      pendingOwnerRequests.delete(requestScope);
      ctx.onChanged();
      return { ok: true, data: {
        intent_id: intent.intent_id, state: intent.state, due_epoch: intent.due_epoch,
        undo_until_epoch: intent.claim_after_epoch,
      } };
    }
    if (!signal.aborted) ctx.onChanged();
    return { ok: false, refusal: {
      code: "publish_already_advanced",
      text: `The publish intent is ${intent.state}. Check its status before trying again.`,
    } };
  } catch {
    if (!signal.aborted) ctx.onChanged();
    return uncertainOwnerAttach();
  }
}

/** `publish.start`: the frame names a run and mode only. The host prepares
 *  one immutable nondispatchable intent while planning. Only its trusted
 *  primary tap opens the server-returned AOS URL and starts one signed attach
 *  attempt; uncertain or refused results never trigger an automatic retry. */
async function planPublishStart(ctx: ActionContext, args: Record<string, unknown>, signal: AbortSignal): Promise<SlotPlan | ActionRefusal> {
  if (signal.aborted) return PUBLISH_STATUS_UNCERTAIN;
  const keys = Object.keys(args).sort().join();
  if (!["mode,run_id", "due_epoch,mode,run_id"].includes(keys) ||
      (args.mode !== "now" && args.mode !== "schedule") ||
      typeof args.run_id !== "string" || !SEGMENT.test(args.run_id))
    return { code: "bad_args", text: "That request is not valid." };
  if (args.mode === "now" ? "due_epoch" in args : !isEpoch(args.due_epoch))
    return { code: "bad_args", text: "That request is not valid." };
  const run = ctx.runs.find(value => value.id === args.run_id && value.install_id === ctx.installId &&
    (!ctx.contextId || (value.context_id ?? "") === ctx.contextId));
  if (!run) return { code: "not_found", text: "That item is not available." };
  if (!publicationBinding(ctx)) return { code: "not_configured", text: "Publishing isn't set up for this app yet. Nothing was scheduled." };
  const mode = args.mode as "now" | "schedule";
  const dueEpoch = args.due_epoch as number | undefined;
  const runId = args.run_id as string;
  const scope = { installId: run.install_id, contextId: run.context_id ?? null };
  const requestScope = JSON.stringify([
    scope.installId, scope.contextId, runId, mode, dueEpoch ?? null,
  ]);
  const request = ownerRequestFor(requestScope);
  if (!request) return PUBLISH_STATUS_UNCERTAIN;

  let prepared: Awaited<ReturnType<typeof socialPublish.prepareIntent>>;
  const prepareAttempt = new AbortController();
  const abortPrepareWithSlot = () => prepareAttempt.abort();
  if (signal.aborted) abortPrepareWithSlot();
  else signal.addEventListener("abort", abortPrepareWithSlot, { once: true });
  const prepareTimeout = setTimeout(() => prepareAttempt.abort(), OWNER_REQUEST_TIMEOUT_MS);
  try {
    prepared = await socialPublish.prepareIntent({ request_id: request, run_id: runId, mode,
      ...(mode === "schedule" ? { due_epoch: dueEpoch as number } : {}) }, scope, prepareAttempt.signal);
  } catch {
    if (!signal.aborted) ctx.onChanged();
    return PUBLISH_STATUS_UNCERTAIN;
  } finally {
    clearTimeout(prepareTimeout);
    signal.removeEventListener("abort", abortPrepareWithSlot);
  }
  if (signal.aborted) return PUBLISH_STATUS_UNCERTAIN;
  const needsOwnerAction = prepared.state === "prepared";
  if (needsOwnerAction && prepared.owner_intent.expires_at * 1_000 <= Date.now()) {
    ctx.onChanged();
    return PUBLISH_STATUS_UNCERTAIN;
  }

  return {
    label: needsOwnerAction ? (mode === "now" ? "Publish now" : "Schedule post") : "Check publish status",
    ...(needsOwnerAction ? { ownerPortalUrl: prepared.owner_action_url } : {}),
    run: async (runSignal: AbortSignal): Promise<ActionResult> => {
      if (runSignal.aborted) return uncertainOwnerAttach();
      const deadline = Math.min(Date.now() + SLOT_TTL_MS, prepared.owner_intent.expires_at * 1_000);
      let shouldAttach = false;

      if (needsOwnerAction) {
        // The browser portal is opened by the same trusted primary gesture.
        // Poll only the advisory status endpoint; pending/unknown never attach.
        while (!runSignal.aborted && Date.now() < deadline) {
          let observed: PreparedOwnerStatus | null = null;
          try {
            observed = await ownerStatusWithTimeout(prepared.prepared_id, scope, runSignal);
          } catch (error) {
            if (runSignal.aborted) return uncertainOwnerAttach();
            if (error instanceof ApiError && (error.status < 500 || error.status === 502)) {
              ctx.onChanged();
              return error.status === 401 || error.status === 403
                ? { ok: false, refusal: NO }
                : { ok: false, refusal: OWNER_REFUSED };
            }
            // A bounded network/transient failure is unknown, not ready.
          }
          if (observed) {
            if (observed.state === "cancelled" || observed.state === "refused" ||
                observed.state === "superseded" || observed.owner_status === "refused") {
              ctx.onChanged();
              return { ok: false, refusal: OWNER_REFUSED };
            }
            if (observed.state === "authorized" && observed.queued) {
              if (observed.queued.claim_armed)
                return reportAttachedIntent(ctx, prepared, observed.queued.intent_id, runSignal, requestScope);
              // An earlier attach committed but did not finish arming. This
              // explicit tap may make its one idempotent local recovery call.
              shouldAttach = true;
              break;
            }
            if (observed.state === "prepared" && observed.owner_status === "ready") {
              shouldAttach = true;
              break;
            }
          }
          const remaining = deadline - Date.now();
          if (remaining <= 0 || !(await delay(Math.min(OWNER_STATUS_POLL_MS, remaining), runSignal)))
            break;
        }
        if (!shouldAttach) {
          if (!runSignal.aborted) ctx.onChanged();
          return uncertainOwnerAttach();
        }
      } else {
        // A recovered AUTHORIZED row is local lifecycle, not a fresh AOS
        // `ready`. Read it before any retry; status itself never arms it.
        let observed: PreparedOwnerStatus;
        try {
          observed = await ownerStatusWithTimeout(prepared.prepared_id, scope, runSignal);
        } catch {
          if (!runSignal.aborted) ctx.onChanged();
          return uncertainOwnerAttach();
        }
        if (runSignal.aborted) return uncertainOwnerAttach();
        if (observed.state === "cancelled" || observed.state === "refused" ||
            observed.state === "superseded" || observed.owner_status === "refused") {
          ctx.onChanged();
          return { ok: false, refusal: OWNER_REFUSED };
        }
        if (observed.state !== "authorized" || !observed.queued) return uncertainOwnerAttach();
        if (observed.queued.claim_armed)
          return reportAttachedIntent(ctx, prepared, observed.queued.intent_id, runSignal, requestScope);
        shouldAttach = true;
      }

      if (!shouldAttach || runSignal.aborted) return uncertainOwnerAttach();
      if (needsOwnerAction && prepared.owner_intent.expires_at * 1_000 <= Date.now()) {
        ctx.onChanged();
        return { ok: false, refusal: OWNER_REFUSED };
      }

      const attempt = new AbortController();
      let timedOut = false;
      const abortWithSlot = () => attempt.abort();
      if (runSignal.aborted) abortWithSlot();
      else runSignal.addEventListener("abort", abortWithSlot, { once: true });
      const timeout = setTimeout(() => { timedOut = true; attempt.abort(); }, OWNER_REQUEST_TIMEOUT_MS);
      let attached: Awaited<ReturnType<typeof socialPublish.attachIntent>>;
      try {
        // Readiness was observed first for a PREPARED action; this accepted
        // primary gesture now makes exactly one attach call. The daemon
        // independently inspects and consumes its own fresh JTI.
        attached = await socialPublish.attachIntent({
          prepared_id: prepared.prepared_id,
          install_id: scope.installId,
          ...(scope.contextId !== null ? { context_id: scope.contextId } : {}),
        }, prepared, attempt.signal);
      } catch {
        if (runSignal.aborted) return uncertainOwnerAttach();
        // Resolve an ambiguous attach with one read; never blindly retry the
        // mutation or infer success from the request having been sent.
        try {
          const resolved = await ownerStatusWithTimeout(prepared.prepared_id, scope, runSignal);
          if (resolved.state === "authorized" && resolved.queued?.claim_armed)
            return reportAttachedIntent(ctx, prepared, resolved.queued.intent_id, runSignal, requestScope);
          if (resolved.owner_status === "refused" || resolved.state === "cancelled") {
            ctx.onChanged();
            return { ok: false, refusal: OWNER_REFUSED };
          }
        } catch { /* leave the local state uncertain and nondispatchable */ }
        if (timedOut || !runSignal.aborted) ctx.onChanged();
        return uncertainOwnerAttach();
      } finally {
        clearTimeout(timeout);
        runSignal.removeEventListener("abort", abortWithSlot);
      }
      if (runSignal.aborted) return uncertainOwnerAttach();
      if (attached.queued.state === "queued" && attached.queued.claim_armed) {
        pendingOwnerRequests.delete(requestScope);
        ctx.onChanged();
        return { ok: true, data: {
          intent_id: attached.queued.intent_id,
          state: attached.queued.state,
          due_epoch: attached.queued.due_epoch,
          undo_until_epoch: attached.queued.claim_after_epoch,
        } };
      }
      return reportAttachedIntent(ctx, prepared, attached.queued.intent_id, runSignal, requestScope);
    },
  };
}

/** `publish.reschedule`: compare-and-swap on the time the screen showed.
 *  The daemon holds the same CAS and the new approval; a stale base or a
 *  claim in between is refused and nothing moves. State is the daemon's
 *  call — the host checks scope only, never liveness, so a stale board read
 *  cannot refuse a valid tap. */
async function planPublishReschedule(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["intent_id", "expected_due_epoch", "due_epoch"]) ||
      typeof args.intent_id !== "string" || !SEGMENT.test(args.intent_id) ||
      !isEpoch(args.expected_due_epoch) || !isEpoch(args.due_epoch))
    return { code: "bad_args", text: "That request is not valid." };
  const intent = inScopeIntent(ctx, args.intent_id);
  if (!intent) return { code: "not_found", text: "That item is not available." };
  const intentId = intent.intent_id;
  const installId = intent.install_id;
  const contextId = intent.context_id;
  const expected = args.expected_due_epoch as number;
  const due = args.due_epoch as number;
  return { label: "Reschedule", run: async () => {
    try {
      await socialPublish.reschedule(intentId, { install_id: installId,
        ...(contextId ? { context_id: contextId } : {}), expected_due_epoch: expected, due_epoch: due });
      ctx.onChanged();
      return { ok: true, data: {} };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

/** `publish.cancel`: one named queued intent, in its own scope. Past queued
 *  the daemon answers `cancel_closed` and nothing is cancelled. */
async function planPublishCancel(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["intent_id"]) || typeof args.intent_id !== "string" || !SEGMENT.test(args.intent_id))
    return { code: "bad_args", text: "That request is not valid." };
  const intent = inScopeIntent(ctx, args.intent_id);
  if (!intent) return { code: "not_found", text: "That item is not available." };
  const intentId = intent.intent_id;
  const installId = intent.install_id;
  const contextId = intent.context_id;
  return { label: "Cancel schedule", run: async () => {
    try {
      await socialPublish.cancel(intentId, installId, contextId);
      ctx.onChanged();
      return { ok: true, data: {} };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

/** `publish.send_now`: the operator's explicit send of one named queued
 *  intent. The daemon claims the row by identity, so a double tap is one
 *  provider call; a refused call leaves the row for a human. */
async function planPublishSendNow(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["intent_id"]) || typeof args.intent_id !== "string" || !SEGMENT.test(args.intent_id))
    return { code: "bad_args", text: "That request is not valid." };
  const intent = inScopeIntent(ctx, args.intent_id);
  if (!intent) return { code: "not_found", text: "That item is not available." };
  const intentId = intent.intent_id;
  const installId = intent.install_id;
  const contextId = intent.context_id;
  return { label: "Send now", run: async () => {
    try {
      await socialPublish.sendNow(intentId, installId, contextId);
      ctx.onChanged();
      return { ok: true, data: {} };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

/** The selector is a UI key only. Save re-reads the account list and the
 *  daemon verifies this exact AOS connection/account tuple again. */
const accountSelector = (row: { connection_id: string; toolkit: string; destination_id: string }) =>
  `${row.connection_id}|${row.toolkit}|${row.destination_id}`;
const isSelectablePublishAccount = (row: { status: string; available: boolean; publishable: boolean; display_name: string }) =>
  row.status === "active" && row.available && row.publishable && row.display_name.trim().length > 0 &&
  [...row.display_name].length <= 80 && !/[\u0000-\u001f\u007f-\u009f]/.test(row.display_name);

async function refreshPublishAccounts(ctx: ActionContext, args: Record<string, unknown>): Promise<ActionResult> {
  if (!exact(args, [])) return refuse("bad_args", "That request is not valid.");
  if (!publicationBinding(ctx))
    return refuse("not_configured", "Publishing isn't set up for this app yet. Nothing was saved.");
  try {
    const rows = await workspaceApps.publishDestinations(ctx.installId, ctx.contextId || undefined);
    const available = rows.filter(isSelectablePublishAccount);
    const counts = new Map<string, number>();
    for (const row of available) {
      const key = accountSelector(row);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    const accounts = available.filter(row => counts.get(accountSelector(row)) === 1).map(row => ({
      destination: accountSelector(row), destination_id: row.destination_id,
      toolkit: row.toolkit, label: row.display_name,
    }));
    return { ok: true, data: { accounts } };
  } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
}

/** Settings accepts only a destination selector and timezone. The host reads
 *  the current account list and binding immediately before saving; labels and
 *  AOS connection ids come from that read. The daemon repeats the AOS lookup,
 *  CASes the current binding revision, and carries a prior grant only when
 *  the complete account identity is unchanged. */
async function savePublishSettings(ctx: ActionContext, args: Record<string, unknown>): Promise<ActionResult> {
  if (!exact(args, ["destination", "timezone"]) ||
      typeof args.destination !== "string" ||
      !(DESTINATION.test(args.destination) || DESTINATION_SELECTOR.test(args.destination)) ||
      typeof args.timezone !== "string" || !ZONE_CHARS.test(args.timezone) || !isZone(args.timezone))
    return refuse("bad_args", "That setting can't be saved.");
  const binding = publicationBinding(ctx);
  if (!binding) return refuse("not_configured", "Publishing isn't set up for this app yet. Nothing was saved.");
  try {
    const [bindings, connections, destinations] = await Promise.all([
      workspaceApps.bindings(ctx.installId, ctx.contextId || undefined),
      workspaceApps.connections(),
      workspaceApps.publishDestinations(ctx.installId, ctx.contextId || undefined),
    ]);
    const current = bindings.find(value => value.id === binding.id && value.install_id === ctx.installId &&
      (value.context_id ?? "") === ctx.contextId && value.slot === "publication" &&
      value.state === "configured" && value.config.bundle_digest === ctx.installation.digest);
    if (!current || current.revision !== binding.revision ||
        current.config.connection_id !== binding.config.connection_id)
      return refuse("stale", "The publishing settings changed. Reload and try again.");
    if (!connections.some(connection => connection.id === current.config.connection_id))
      return refuse("not_configured", "The bound connection is unavailable. Nothing was saved.");
    const wanted = args.destination as string;
    const available = destinations.filter(isSelectablePublishAccount);
    const matches = available.filter(row =>
      wanted === accountSelector(row) || (DESTINATION.test(wanted) && wanted === row.destination_id));
    const selected = matches[0];
    if (matches.length !== 1 || !selected)
      return refuse("bad_destination", "That account is unavailable or ambiguous. Refresh accounts and try again.");
    const saved = await workspaceApps.setBindingPublish(ctx.installId, current.id, {
      expected_revision: current.revision,
      destination_id: selected.destination_id,
      destination_label: selected.display_name,
      toolkit: selected.toolkit,
      timezone: args.timezone as string,
      aos_connection_id: selected.connection_id,
    });
    ctx.onChanged();
    return { ok: true, data: { revision: saved.revision } };
  } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
}
