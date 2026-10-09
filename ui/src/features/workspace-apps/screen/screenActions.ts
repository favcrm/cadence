import { ApiError } from "../../../lib/api";
import { navigate } from "../../../lib/useLocation";
import { rememberContext } from "../contextSelection";
import { workspaceApps, type AppContext, type CapabilityQuote, type Installation, type WorkspaceRun } from "../workspaceApps";
import { contextDefaultKeys, instagramLink } from "./screenProjection";
import { accountLabel, connectReturnTo, isGrantId, publicationBinding, sendSlot } from "../socialConnect";
import type { ActionRefusal, ActionResult, CallVerb, SlotVerb } from "./screenProtocol";
import type { Planner, SlotPlan } from "./screenSlot";

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
  /** Reload the board's data after a write. */
  onChanged: () => void | Promise<void>;
}

const NO: ActionRefusal = { code: "denied", text: "Only the operator can do that." };
const refuse = (code: string, text: string): ActionResult => ({ ok: false, refusal: { code, text } });

/** CAD-1177 — invoke one declared tool alias through the host broker.
 *  The mount's action token (`MountReceipt.action_token`) is the only
 *  mount-scoped proof the host holds; it is never sent to the frame.
 *  The daemon re-proves session + install/digest + declared alias→slot +
 *  read/draft effect before any broker/provider work. */
export async function invokeTool(
  actionToken: string,
  alias: string,
  input: Record<string, unknown>,
  requestId: string,
  generationScope?:import("./screenProtocol").GenerationScope,
): Promise<ActionResult> {
  try {
    const out = await workspaceApps.invokeTool({
      action_token: actionToken, alias, input, request_id: requestId,
      ...(generationScope?{generation_scope:generationScope}:{}),
    });
    return { ok: true, data: out };
  } catch (error) {
    return { ok: false, refusal: plainRefusal(error) };
  }
}

/** CAD-1177 — retire a mount's action handle when the host tears the
 *  frame down or remounts. Best-effort: the daemon's consume-time
 *  supersede already refuses a stale mount, so a failed revoke only
 *  leaves a handle its own TTL/session gate still bounds. */
export async function revokeTool(actionToken: string): Promise<void> {
  try {
    await workspaceApps.revokeTool({ action_token: actionToken });
  } catch {
    // Teardown is best-effort; the daemon still refuses stale mounts.
  }
}
const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const exact = (value: Record<string, unknown>, keys: string[]) => Object.keys(value).sort().join() === [...keys].sort().join();

/** A daemon refusal, as a plain line. The daemon's wording never reaches the frame. */
export function plainRefusal(error: unknown): ActionRefusal {
  if (!(error instanceof ApiError)) return { code: "failed", text: "That didn't work. Nothing was started." };
  if (error.status === 401 || error.status === 403) return NO;
  const text = error.message.toLowerCase();
  if (text.includes("price_changed")) return { code: "price_changed", text: "Something changed. Please try again." };
  if (text.includes("no default team")) return { code: "team_missing", text: "Set this app's team in Settings first." };
  if (text.includes("worker")) return { code: "team_unavailable", text: "A team member is unavailable right now." };
  if (text.includes("stale")) return { code: "stale", text: "That changed somewhere else. Reload and try again." };
  // A missing binding is a setup problem the app can name, not a bad input.
  if (text.includes("capability binding is absent")) return { code: "binding_absent", text: "Finish setting this app up in Settings first." };
  if (error.status === 409) return { code: "refused", text: "Cadence couldn't do that. Nothing was started." };
  return { code: "failed", text: "That didn't work. Nothing was started." };
}

const randomRequest = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("");

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
  // Re-read the board first, so the new context is in its data when it becomes
  // the selection: the screen then remounts once, with that context.
  await ctx.onChanged();
  // The defaults live in this context: make it the board's selection so the
  // frame (and the runs it starts) read and use them.
  rememberContext(ctx.installId, saved.id);
  return { ok: true, data: { context_id: saved.id, revision: saved.revision } };
}

/** `open-link`: https on allowlisted hosts only. The host shows a link the
 *  operator clicks (the SafeLink policy); the frame cannot navigate anything. */
async function socialDraftCall(verb: CallVerb,args:Record<string,unknown>,ui:{actionToken?:string}):Promise<ActionResult>{
  const actionToken=ui.actionToken;
  const alias=args.alias;
  if(typeof actionToken!=="string" || !/^[a-f0-9]{64}$/.test(actionToken) || typeof alias!=="string") return refuse("denied","That draft action is unavailable.");
  const common={action_token:actionToken,alias};
  let operation:"create"|"list"|"show"|"update"|"sources/show"|"sources/save"|"effect-stage";
  let fields:Record<string,unknown>;
  switch(verb){
    case "social.drafts.list": operation="list"; if(!exact(args,["alias"])) return refuse("bad_args","That request is not valid."); fields={}; break;
    case "social.drafts.show": operation="show"; if(!exact(args,["alias","draft_id"])||typeof args.draft_id!=="string") return refuse("bad_args","That request is not valid."); fields={draft_id:args.draft_id}; break;
    case "social.drafts.create": operation="create"; if(!exact(args,["alias","request_id","caption","source"])&&!exact(args,["alias","request_id","caption","source","asset_id"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,caption:args.caption,source:args.source,...("asset_id" in args?{asset_id:args.asset_id}:{})}; break;
    case "social.drafts.update": operation="update"; if(!exact(args,["alias","request_id","draft_id","expected_revision","caption"])&&!exact(args,["alias","request_id","draft_id","expected_revision","caption","asset_id"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,draft_id:args.draft_id,expected_revision:args.expected_revision,caption:args.caption,...("asset_id" in args?{asset_id:args.asset_id}:{})}; break;
    case "social.drafts.publish.stage": operation="effect-stage"; if(!exact(args,["alias","draft_id","revision","request_id"])||typeof args.draft_id!=="string"||!Number.isSafeInteger(args.revision)||typeof args.request_id!=="string") return refuse("bad_args","That request is not valid."); fields={proof:{kind:"social_draft",draft_id:args.draft_id,revision:args.revision},request_id:args.request_id}; break;
    case "social.sources.show": operation="sources/show"; if(!exact(args,["alias"])) return refuse("bad_args","That request is not valid."); fields={}; break;
    case "social.sources.save": operation="sources/save"; if(!exact(args,["alias","request_id","expected_revision","handles"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,expected_revision:args.expected_revision,handles:args.handles}; break;
    default: return refuse("bad_args","That request is not valid.");
  }
  try { const data=await workspaceApps.socialDraftAction(operation,{...common,...fields}); return {ok:true,data}; }
  catch(error){return {ok:false,refusal:plainRefusal(error)};}
}

/** Board paths an app may send the operator to. Exact paths only, never a caller-built route. */
export const BOARD_LINK_PATHS: readonly string[] = ["/settings/connections"];
/** The visible target of an external link: its host plus path, which is what the confirm names. */
export const linkTarget = (href: string): string => { const url = new URL(href); return url.host + url.pathname + url.search; };

/** CAD-1290: on a hosted workspace the app's "Manage connections" link opens the AgenticOS Instagram
 *  connect page top-level. The host composes that URL (the frame never builds it); a local board, or
 *  any failure, keeps the local connections page. */
async function connectOrOpen(ctx: ActionContext, args: Record<string, unknown>, ui: { showLink(url: string): void; navigate?: (path: string) => void; assign?: (url: string) => void }): Promise<ActionResult> {
  if (args.url === "/settings/connections") {
    try {
      const reply = await workspaceApps.connectLink(ctx.installId, connectReturnTo(window.location.origin, ctx.installId));
      if (reply.hosted && reply.url) { (ui.assign ?? (url => window.location.assign(url)))(reply.url); return { ok: true, data: {} }; }
    } catch { /* keep the local page */ }
  }
  return openLink(args, ui);
}

/** `social.destinations.list`: the company's connected Instagram accounts. Read only. */
async function listDestinations(args: Record<string, unknown>, ctx: ActionContext): Promise<ActionResult> {
  if (!exact(args, [])) return refuse("bad_args", "That request is not valid.");
  const reply = await workspaceApps.destinations(ctx.installId);
  return { ok: true, data: { unavailable: reply.unavailable, destinations: reply.destinations.map(value => ({ destination_id: value.destination_id, label: accountLabel(value.label || value.destination_id) })) } };
}

/** `publish.destination.use`: a host-drawn tap points the install's publication binding at one of the
 *  company's accounts. The label comes from the company's list, never from the frame. */
async function planUseDestination(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["destination_id", "grant_id"]) || typeof args.destination_id !== "string" || typeof args.grant_id !== "string"
      || !isGrantId(args.grant_id)) return { code: "bad_args", text: "That request is not valid." };
  const slot = sendSlot(ctx.installation);
  if (!slot) return { code: "unknown_slot", text: "That isn't available." };
  const { destination_id: destination, grant_id: grant } = args;
  try {
    const listed = await workspaceApps.destinations(ctx.installId);
    const chosen = listed.destinations.find(value => value.destination_id === destination);
    if (!chosen) return { code: "unknown_destination", text: "That Instagram account isn't connected." };
    const contextId = ctx.contextId;
    const binding = publicationBinding(await workspaceApps.bindings(ctx.installId, contextId || undefined), slot, contextId || null, ctx.installation.digest);
    const request = randomRequest();
    return { label: `Use ${accountLabel(chosen.label || destination)} for publishing`, run: async () => {
      try {
        await workspaceApps.useDestination(ctx.installId, { destination_id: destination, grant_id: grant,
          ...(contextId ? { context_id: contextId } : {}),
          ...(binding ? { expected_revision: binding.revision } : { request_id: request }) });
        ctx.onChanged();
        return { ok: true, data: {} };
      } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
    } };
  } catch (error) { return plainRefusal(error); }
}

function openLink(args: Record<string, unknown>, ui: { showLink(url: string): void; navigate?: (path: string) => void }): ActionResult {
  if (!exact(args, ["url"]) || typeof args.url !== "string") return refuse("bad_args", "That link is not valid.");
  // A same-origin board path moves the board's router; an external https link
  // is only shown, and the operator confirms its visible target before it opens.
  if (BOARD_LINK_PATHS.includes(args.url)) { (ui.navigate ?? navigate)(args.url); return { ok: true, data: {} }; }
  const url = instagramLink(args.url);
  if (!url) return refuse("link_blocked", "That link can't be opened here.");
  ui.showLink(url);
  return { ok: true, data: {} };
}

/** The direct (non-spending) verbs. The set is closed in `screenProtocol`. */
export function runCall(ctx: ActionContext, verb: CallVerb, args: Record<string, unknown>,
  ui: { showLink(url: string): void; navigate?: (path: string) => void; assign?: (url: string) => void; actionToken?: string }): Promise<ActionResult> {
  const table: Record<CallVerb, () => Promise<ActionResult> | ActionResult> = {
    "read.run": () => readRun(ctx, args),
    "context.defaults.save": () => saveDefaults(ctx, args),
    "open-link": () => connectOrOpen(ctx, args, ui),
    "social.destinations.list": () => listDestinations(args, ctx),
    "social.drafts.list": () => socialDraftCall(verb,args,ui),
    "social.drafts.show": () => socialDraftCall(verb,args,ui),
    "social.drafts.create": () => socialDraftCall(verb,args,ui),
    "social.drafts.update": () => socialDraftCall(verb,args,ui),
    "social.drafts.publish.stage": () => socialDraftCall(verb,args,ui),
    "social.sources.show": () => socialDraftCall(verb,args,ui),
    "social.sources.save": () => socialDraftCall(verb,args,ui),
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
  if (!["inputs,workflow", "inputs,selected_post_id,source_receipt_id,workflow"].includes(keys) ||
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
        expected_quotes: expected, ...(contextId ? { context_id: contextId } : {}), ...source });
      ctx.onChanged();
      return { ok: true, data: {} };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

/** The spend/publish verbs. HP4 adds `publish.*` here; the set is closed in `screenProtocol`. */
export function makePlanner(get: () => ActionContext): Planner {
  return (verb: SlotVerb, args) => {
    const table: Record<SlotVerb, () => Promise<SlotPlan | ActionRefusal>> = {
      "run.start": () => planRunStart(get(), args),
      "publish.destination.use": () => planUseDestination(get(), args),
    };
    return table[verb]();
  };
}
