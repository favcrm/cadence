import { ApiError } from "../../../lib/api";
import { withPrivateOperation } from "../../../lib/privateSession";
import { navigate } from "../../../lib/useLocation";
import { rememberContext } from "../contextSelection";
import { workspaceApps, type AppContext, type CapabilityQuote, type Installation, type WorkspaceRun } from "../workspaceApps";
import { contextDefaultKeys, instagramLink } from "./screenProjection";
import { accountLabel, connectReturnTo, publicationBinding, sendSlot } from "../socialConnect";
import { TOOL_ALIAS, CALL_VERBS, SLOT_VERBS, SCREEN_DRAWER_OPEN, SCREEN_DRAWER_CLOSE, parseScreenDrawerCall, mediaChatAttachArgs, mediaPublishArgs, mediaReadArgs, originalDownloadArgs, parseRunSlotReceipt, RUN_REQUEST_ID, type ActionRefusal, type ActionResult, type CallVerb, type SlotVerb } from "./screenProtocol";
import type { ScreenActions } from "./screenLifecycle";
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
  /** Select a context after its defaults were successfully saved. */
  onSavedContext?: (saved: AppContext) => void;
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
  if (error.code === "draft_in_use") return { code: "draft_in_use", text: "This draft is approved or published, so it can't be discarded." };
  if (error.code === "stale_revision") return { code: "stale_revision", text: "That changed somewhere else. Reload and try again." };
  if (error.code === "draft_not_found") return { code: "draft_not_found", text: "That draft is no longer here." };
  const text = error.message.toLowerCase();
  if (text.includes("320 pixels")) return { code: "image_too_small", text: "Use a photo at least 320 pixels on each side." };
  if (text.includes("4:5 to 1.91:1")) return { code: "image_aspect", text: "Use a photo between 4:5 and 1.91:1." };
  if (text.includes("must be jpeg or png") || text.includes("not an intact owned jpeg or png") || text.includes("no supported image signature")) return { code: "image_type", text: "Use a JPEG or PNG photo." };
  if (text.includes("outside the custody size") || text.includes("outside the size bound") || text.includes("does not fit the custody bound")) return { code: "image_too_large", text: "That photo is too large. Use one under 2 MB." };
  if (text.includes("cannot be decoded") || text.includes("invalid or exceed the decode")) return { code: "image_unreadable", text: "That photo can't be read. Try a different one." };
  if (text.includes("price_changed")) return { code: "price_changed", text: "Something changed. Please try again." };
  if (text.includes("no default team")) return { code: "team_missing", text: "Set this app's team in Settings first." };
  if (text.includes("worker")) return { code: "team_unavailable", text: "A team member is unavailable right now." };
  if (text.includes("stale")) return { code: "stale", text: "That changed somewhere else. Reload and try again." };
  // A missing binding is a setup problem the app can name, not a bad input.
  if (text.includes("capability binding is absent")) return { code: "binding_absent", text: "Finish setting this app up in Settings first." };
  // A zero-context install is a setup state, not a failed action.
  if (text.includes("mounted active context")) return { code: "context_missing", text: "Set up this app's workspace context first, then try again." };
  if (error.status === 409) return { code: "refused", text: "Cadence couldn't do that. Nothing was started." };
  return { code: "failed", text: "That didn't work. Nothing was started." };
}

const randomRequest = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, "0")).join("");

/** CAD-1365 — `media.read`: the workspace catalog metadata page through
 *  the mount's own action token (never a frame-supplied credential).
 *  Re-validates the closed input even though the port parser already
 *  did — the table may be invoked on a direct call path. The reply is
 *  the daemon's bounded projection: catalog rows, `next_cursor` and the
 *  tagged `current_actor` — never original locators, bytes or secrets. */
async function mediaRead(args: Record<string, unknown>, ui: { actionToken?: string }): Promise<ActionResult> {
  const checked = mediaReadArgs(args);
  const actionToken = ui.actionToken;
  if (!checked || typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken))
    return refuse("bad_args", "That request is not valid.");
  try {
    const out = await workspaceApps.mediaRead({ action_token: actionToken, input: checked });
    return { ok: true, data: out };
  } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
}

/** A frame succeeds only after the real host draft accepts its image. */
async function mediaChatAttach(args: Record<string, unknown>, ui: Pick<Parameters<ScreenActions["call"]>[2], "actionToken" | "attachMedia">): Promise<ActionResult> {
  const checked = mediaChatAttachArgs(args);
  const actionToken = ui.actionToken;
  if (!checked || typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken))
    return refuse("bad_args", "That request is not valid.");
  if (!ui.attachMedia) return refuse("unavailable", "The current chat draft is not ready for an image.");
  try {
    const out = await ui.attachMedia({
      asset_id: checked.asset_id as string,
      expected_revision: checked.expected_revision as number,
    }, actionToken);
    if (out.asset_id !== checked.asset_id || out.revision !== checked.expected_revision)
      return refuse("failed", "The host did not accept that image.");
    return { ok: true, data: { asset_id: out.asset_id, revision: out.revision } };
  } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
}

/** CAD-1365 — `media.publish`: the frozen catalog curation intent
 *  `{asset_id, expected_revision, request_id}` only. It mutates the
 *  workspace catalog's own metadata (draft → published) — no external
 *  posting, provider work or charge, so it rides the call surface like
 *  the other metadata verbs rather than a host-drawn slot. The daemon
 *  re-proves the mount, the declared `media.publish` permission and the
 *  atomic replay/CAS on every call. */
async function mediaPublish(args: Record<string, unknown>, ui: { actionToken?: string }): Promise<ActionResult> {
  const checked = mediaPublishArgs(args);
  const actionToken = ui.actionToken;
  if (!checked || typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken))
    return refuse("bad_args", "That request is not valid.");
  try {
    const out = await workspaceApps.mediaPublish({
      action_token: actionToken,
      asset_id: checked.asset_id as string,
      expected_revision: checked.expected_revision as number,
      request_id: checked.request_id as string,
    });
    return { ok: true, data: out };
  } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
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
/** Save the content defaults into the selected active context (or the
 *  installation's "Default" one when none is selected), creating "Default" when there is none. `expected_revision` is the revision the
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
  // The selected context is where the frame's defaults live (its revision is what the frame saw);
  // only with no selection does the "Default" context stand in.
  const selected = ctx.contextId ? contexts.find(value => value.id === ctx.contextId && value.install_id === ctx.installId) : undefined;
  if (ctx.contextId && (!selected || selected.state !== "active")) return refuse("not_found", "That item is not available.");
  const current = selected ?? contexts.find(value => mine(value) && value.state === "active");
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
      label: current.config.label, input_defaults: { ...current.config.input_defaults, ...values } });
  }
  // Re-read the board first, so the new context is in its data when it becomes
  // the selection: the screen then remounts once, with that context.
  await ctx.onChanged();
  // The defaults live in this context: make it the board's selection so the
  // frame (and the runs it starts) read and use them.
  if (ctx.onSavedContext) ctx.onSavedContext(saved);
  else rememberContext(ctx.installId, saved.id);
  return { ok: true, data: { context_id: saved.id, revision: saved.revision } };
}

/** `open-link`: https on allowlisted hosts only. The host shows a link the
 *  operator clicks (the SafeLink policy); the frame cannot navigate anything. */
async function socialDraftCall(verb: CallVerb,args:Record<string,unknown>,ui:{actionToken?:string}):Promise<ActionResult>{
  const actionToken=ui.actionToken;
  // Discard names no alias (CAD-1303): the draft tool alias is the app's `social.draft`.
  const alias=verb==="social.drafts.discard"?"social.draft":args.alias;
  if(typeof actionToken!=="string" || !/^[a-f0-9]{64}$/.test(actionToken) || typeof alias!=="string") return refuse("denied","That draft action is unavailable.");
  const common={action_token:actionToken,alias};
  let operation:"create"|"list"|"show"|"update"|"discard"|"sources/show"|"sources/save"|"effect-stage";
  let fields:Record<string,unknown>;
  switch(verb){
    case "social.drafts.list": operation="list"; if(!exact(args,["alias"])) return refuse("bad_args","That request is not valid."); fields={}; break;
    case "social.drafts.show": operation="show"; if(!exact(args,["alias","draft_id"])||typeof args.draft_id!=="string") return refuse("bad_args","That request is not valid."); fields={draft_id:args.draft_id}; break;
    case "social.drafts.create": operation="create"; if(!exact(args,["alias","request_id","caption","source"])&&!exact(args,["alias","request_id","caption","source","asset_id"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,caption:args.caption,source:args.source,...("asset_id" in args?{asset_id:args.asset_id}:{})}; break;
    case "social.drafts.update": operation="update"; if(!exact(args,["alias","request_id","draft_id","expected_revision","caption"])&&!exact(args,["alias","request_id","draft_id","expected_revision","caption","asset_id"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,draft_id:args.draft_id,expected_revision:args.expected_revision,caption:args.caption,...("asset_id" in args?{asset_id:args.asset_id}:{})}; break;
    case "social.drafts.discard": operation="discard"; if(!exact(args,["draft_id","revision"])||typeof args.draft_id!=="string"||!Number.isSafeInteger(args.revision)) return refuse("bad_args","That request is not valid."); fields={draft_id:args.draft_id,revision:args.revision}; break;
    case "social.drafts.publish.stage": operation="effect-stage"; if(!exact(args,["alias","draft_id","revision","request_id"])||typeof args.draft_id!=="string"||!Number.isSafeInteger(args.revision)||typeof args.request_id!=="string") return refuse("bad_args","That request is not valid."); fields={proof:{kind:"social_draft",draft_id:args.draft_id,revision:args.revision},request_id:args.request_id}; break;
    case "social.sources.show": operation="sources/show"; if(!exact(args,["alias"])) return refuse("bad_args","That request is not valid."); fields={}; break;
    case "social.sources.save": operation="sources/save"; if(!exact(args,["alias","request_id","expected_revision","handles"])) return refuse("bad_args","That request is not valid."); fields={request_id:args.request_id,expected_revision:args.expected_revision,handles:args.handles}; break;
    default: return refuse("bad_args","That request is not valid.");
  }
  try { const data=await workspaceApps.socialDraftAction(operation,{...common,...fields}); return {ok:true,data}; }
  catch(error){return {ok:false,refusal:plainRefusal(error)};}
}

/** Board paths an app may send the operator to. Exact paths only, never a caller-built route. */
/** CAD-1615 / CAD-1777 — the app asks the board to present its screen in the
 *  board-wide drawer. The host owns the geometry (whole window height, the
 *  board under a scrim); the surface the app draws there is the app's. The
 *  arguments are the protocol's one closed shape, so a malformed request
 *  changes nothing on either the generic or the records dispatch path. */
function openScreenDrawer(args: Record<string, unknown>, ui: Parameters<ScreenActions["call"]>[2]): ActionResult {
  const request = parseScreenDrawerCall(SCREEN_DRAWER_OPEN, args);
  if (!request || request.mode !== "drawer") return refuse("bad_args", "That request is not valid.");
  if (!ui.openScreenDrawer) return refuse("unavailable", "This app cannot open a board drawer here.");
  ui.openScreenDrawer(request.surface);
  return { ok: true, data: request };
}
function closeScreenDrawer(args: Record<string, unknown>, ui: Parameters<ScreenActions["call"]>[2]): ActionResult {
  const request = parseScreenDrawerCall(SCREEN_DRAWER_CLOSE, args);
  if (!request) return refuse("bad_args", "That request is not valid.");
  if (!ui.closeScreenDrawer) return refuse("unavailable", "This app cannot open a board drawer here.");
  ui.closeScreenDrawer();
  return { ok: true, data: request };
}

export const BOARD_LINK_PATHS: readonly string[] = ["/settings/connections"];
/** The visible target of an external link: its host plus path, which is what the confirm names. */
export const linkTarget = (href: string): string => { const url = new URL(href); return url.host + url.pathname + url.search; };

/** CAD-1290: on a hosted workspace the app's "Manage connections" link opens the AgenticOS Instagram
 *  connect page top-level. The host composes that URL (the frame never builds it); a local board, or
 *  a failure while this continuation is still current, keeps the local connections page.
 *  Pause, reset, and frame close abort the continuation for good. A later response cannot assign,
 *  navigate, or show a link, and re-admission does not revive it. */
async function connectOrOpen(ctx: ActionContext, args: Record<string, unknown>, ui: { showLink(url: string): void; navigate?: (path: string) => void; assign?: (url: string) => void; signal?: AbortSignal }): Promise<ActionResult> {
  return withPrivateOperation(async (operation) => {
    if (args.url === "/settings/connections") {
      try {
        // CAD-1438: the screen only runs under `/apps/<key>` (uniqueness proven
        // at route level), so the installation's own key is its return path.
        const reply = await workspaceApps.connectLink(ctx.installId, connectReturnTo(window.location.origin, ctx.installation.name, ctx.installId), operation.signal);
        operation.assertCurrent();
        if (reply.hosted && reply.url) { (ui.assign ?? (url => window.location.assign(url)))(reply.url); return { ok: true, data: {} }; }
        // Hosted but no public AgenticOS address: say so, never send the owner to the local page.
        if (reply.hosted) return refuse("unavailable", "Instagram connect isn't available for this workspace yet.");
      } catch {
        operation.assertCurrent();
      }
    }
    operation.assertCurrent();
    return openLink(args, ui);
  }, ui.signal);
}

/** `social.destinations.list`: the company's connected Instagram accounts. Read only. */
async function listDestinations(args: Record<string, unknown>, ctx: ActionContext): Promise<ActionResult> {
  if (!exact(args, [])) return refuse("bad_args", "That request is not valid.");
  const reply = await workspaceApps.destinations(ctx.installId);
  return { ok: true, data: { unavailable: reply.unavailable, destinations: reply.destinations.map(value => ({ destination_id: value.destination_id, label: accountLabel(value.label || value.destination_id) })) } };
}

/** `publish.destination.use`: the install's own publication binding points at one of the
 *  company's accounts. The account comes from the company's list, never from the frame. */
async function planUseDestination(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["destination_id"]) || typeof args.destination_id !== "string") return { code: "bad_args", text: "That request is not valid." };
  const slot = sendSlot(ctx.installation);
  if (!slot) return { code: "unknown_slot", text: "That isn't available." };
  const destination = args.destination_id;
  try {
    const listed = await workspaceApps.destinations(ctx.installId);
    const chosen = listed.destinations.find(value => value.destination_id === destination);
    if (!chosen) return { code: "unknown_destination", text: "That Instagram account isn't connected." };
    const contextId = ctx.contextId;
    const binding = publicationBinding(await workspaceApps.bindings(ctx.installId, contextId || undefined), slot, contextId || null, ctx.installation.digest);
    const request = randomRequest();
    return { label: `Use ${accountLabel(chosen.label || destination)} for publishing`, run: async (): Promise<ActionResult> => {
      try {
        await workspaceApps.useDestination(ctx.installId, { destination_id: destination,
          ...(contextId ? { context_id: contextId } : {}),
          ...(binding ? { expected_revision: binding.revision } : { request_id: request }) });
        ctx.onChanged();
        return { ok: true, data: {} };
      } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
    } };
  } catch (error) { return plainRefusal(error); }
}

async function planSavePublishSettings(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  if (!exact(args, ["destination_id", "timezone"]) || typeof args.destination_id !== "string" ||
      typeof args.timezone !== "string" || args.timezone.length === 0 || args.timezone.length > 64)
    return { code: "bad_args", text: "That request is not valid." };
  try { new Intl.DateTimeFormat("en", { timeZone: args.timezone }); }
  catch { return { code: "bad_timezone", text: "Choose a valid time zone." }; }
  const slot = sendSlot(ctx.installation);
  if (!slot) return { code: "unknown_slot", text: "That isn't available." };
  try {
    const listed = await workspaceApps.destinations(ctx.installId);
    const chosen = listed.destinations.find(value => value.destination_id === args.destination_id);
    if (!chosen || listed.unavailable) return { code: "unknown_destination", text: "That publishing account isn't available." };
    const binding = publicationBinding(await workspaceApps.bindings(ctx.installId, ctx.contextId || undefined), slot,
      ctx.contextId || null, ctx.installation.digest);
    const body = { destination_id: chosen.destination_id, timezone: args.timezone,
      ...(ctx.contextId ? { context_id: ctx.contextId } : {}),
      ...(binding ? { expected_revision: binding.revision } : { request_id: randomRequest() }) };
    return { label: `Save time zone · ${accountLabel(chosen.label || chosen.destination_id)} · ${args.timezone}`, run: async (): Promise<ActionResult> => {
      try {
        await workspaceApps.useDestination(ctx.installId, body);
        await ctx.onChanged();
        return { ok: true, data: {} };
      } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
    } };
  } catch (error) { return plainRefusal(error); }
}

/** `publish.draft.confirm`: the human confirmed the frozen draft's publish in the app.
 *  The human's own authenticated action is the approval, so this sends the board's frozen effect
 *  directly — no operator card and no second approval round. */
async function planConfirmPublish(ctx: ActionContext, args: Record<string, unknown>, actionToken?: string): Promise<SlotPlan | ActionRefusal> {
  if (typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken)) return { code: "denied", text: "That draft action is unavailable." };
  if (!exact(args, ["alias", "effect_id"]) || typeof args.alias !== "string" || typeof args.effect_id !== "string" || !TOOL_ALIAS.test(args.alias))
    return { code: "bad_args", text: "That request is not valid." };
  return { label: "Publish now", run: async (): Promise<ActionResult> => {
    try {
      const sent = await workspaceApps.socialDraftAction("confirm", { action_token: actionToken, alias: args.alias as string, effect_id: args.effect_id as string });
      await ctx.onChanged();
      // The board answers 200 with the effect it settled; only a posted effect is success.
      const effect = (sent as { effect?: { state?: unknown } }).effect;
      const state = typeof effect?.state === "string" ? effect.state : "";
      if (state === "posted") return { ok: true, data: sent };
      if (state === "failed") return { ok: false, refusal: { code: "refused", text: "Instagram didn't take that post." } };
      return { ok: false, refusal: { code: "unconfirmed", text: "That isn't confirmed yet. Check before trying again." } };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
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

/** Execute a planner as a direct call: the same validation and daemon write,
 *  with no host-drawn tap. */
async function runPlan(plan: SlotPlan | ActionRefusal): Promise<ActionResult> {
  return "code" in plan ? { ok: false, refusal: plan } : plan.run();
}

/** The direct (non-spending) verbs. The set is closed in `screenProtocol`. */
export function runCall(ctx: ActionContext, verb: CallVerb, args: Record<string, unknown>,
  ui: Parameters<ScreenActions["call"]>[2] & { navigate?: (path: string) => void; assign?: (url: string) => void }): Promise<ActionResult> {
  if (!(CALL_VERBS as readonly string[]).includes(verb)) return Promise.resolve(refuse("forbidden", "This operation requires the records service."));
  const table: Record<(typeof CALL_VERBS)[number], () => Promise<ActionResult> | ActionResult> = {
    "read.run": () => readRun(ctx, args),
    "context.defaults.save": () => saveDefaults(ctx, args),
    "open-link": () => connectOrOpen(ctx, args, ui),
    "screen.drawer.open": () => openScreenDrawer(args, ui),
    "screen.drawer.close": () => closeScreenDrawer(args, ui),
    "social.destinations.list": () => listDestinations(args, ctx),
    "social.drafts.list": () => socialDraftCall(verb,args,ui),
    "social.drafts.show": () => socialDraftCall(verb,args,ui),
    "social.drafts.create": () => socialDraftCall(verb,args,ui),
    "social.drafts.update": () => socialDraftCall(verb,args,ui),
    "social.drafts.discard": () => socialDraftCall(verb,args,ui),
    "social.drafts.publish.stage": () => socialDraftCall(verb,args,ui),
    "social.sources.show": () => socialDraftCall(verb,args,ui),
    "social.sources.save": () => socialDraftCall(verb,args,ui),
    "publish.destination.use": async () => runPlan(await planUseDestination(ctx, args)),
    "publish.settings.save": async () => runPlan(await planSavePublishSettings(ctx, args)),
    "publish.draft.confirm": async () => runPlan(await planConfirmPublish(ctx, args, ui.actionToken)),
    "publish.draft.schedule": async () => runPlan(await planDraftEffectChange(ctx, verb, args, ui.actionToken)),
    "publish.draft.reschedule": async () => runPlan(await planDraftEffectChange(ctx, verb, args, ui.actionToken)),
    "publish.draft.cancel": async () => runPlan(await planDraftEffectChange(ctx, verb, args, ui.actionToken)),
    "media.read": () => mediaRead(args, ui),
    "media.chat.attach": () => mediaChatAttach(args, ui),
    "media.publish": () => mediaPublish(args, ui),
  };
  return Promise.resolve().then(table[verb as (typeof CALL_VERBS)[number]]).catch((error: unknown): ActionResult => ({ ok: false, refusal: plainRefusal(error) }));
}

const MAX_DRAFT_SCHEDULE_SECONDS = 366 * 24 * 60 * 60;

/** Schedule controls name a board draft effect and execute on the human's own action: the app's
 *  time picker already chose the time, so no second confirmation card is shown. */
async function planDraftEffectChange(ctx: ActionContext, verb: CallVerb, args: Record<string, unknown>, actionToken?: string): Promise<SlotPlan | ActionRefusal> {
  if (typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken)) return { code: "denied", text: "That draft action is unavailable." };
  const hasDue = verb !== "publish.draft.cancel";
  const expectedKeys = hasDue ? ["alias", "due_epoch", "effect_id"] : ["alias", "effect_id"];
  if (!exact(args, expectedKeys) || typeof args.alias !== "string" || !TOOL_ALIAS.test(args.alias) ||
      typeof args.effect_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(args.effect_id) ||
      (hasDue && (!Number.isSafeInteger(args.due_epoch) || (args.due_epoch as number) <= 0)))
    return { code: "bad_args", text: "That request is not valid." };
  const dueEpoch = hasDue ? args.due_epoch as number : undefined;
  const now = Math.floor(Date.now() / 1000);
  if (dueEpoch !== undefined && (dueEpoch <= now || dueEpoch > now + MAX_DRAFT_SCHEDULE_SECONDS))
    return { code: "bad_due_epoch", text: "Choose a future time within 366 days." };
  const operation = verb === "publish.draft.schedule" ? "schedule" : verb === "publish.draft.reschedule" ? "reschedule" : "cancel";
  const label = verb === "publish.draft.schedule" ? "Schedule post" : verb === "publish.draft.reschedule" ? "Reschedule post" : "Cancel scheduled post";
  return { label, run: async (): Promise<ActionResult> => {
    try {
      const changed = await workspaceApps.socialDraftAction(operation, { action_token: actionToken, alias: args.alias as string, effect_id: args.effect_id as string, ...(dueEpoch !== undefined ? { due_epoch: dueEpoch } : {}) });
      await ctx.onChanged();
      return { ok: true, data: changed };
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
  } };
}

const LABEL_MAX = 40;
const INPUT_MAX = 16;
const VALUE_MAX = 8192;
/** CAD-1365 — a native start whose reply or outcome cannot be confirmed is
 *  honestly uncertain, never a claim that nothing started. */
const UNCONFIRMED: ActionRefusal = { code: "unconfirmed", text: "Cadence could not confirm this run. Check the library before trying again." };
/** One native quote's terms, checked before any confirmation or start and
 *  copied to primitives (never the SDK's mutable object): schema 1 USD,
 *  safe-integer non-negative micros, units >= 1, unit*units == total, and a
 *  bounded non-empty price_revision. `null` refuses — no label, no start. */
function checkedQuote(quote: CapabilityQuote["quote"]): CapabilityQuote["quote"] | null {
  if (!isObject(quote)) return null;
  const q = quote as Record<string, unknown>;
  if (q.schema !== 1 || q.currency !== "USD") return null;
  const { unit_price_micros: unit, units, total_price_micros: total, price_revision: revision } = q;
  if (typeof unit !== "number" || !Number.isSafeInteger(unit) || unit < 0) return null;
  if (typeof units !== "number" || !Number.isSafeInteger(units) || units < 1) return null;
  if (typeof total !== "number" || !Number.isSafeInteger(total) || total < 0 || unit * units !== total) return null;
  if (typeof revision !== "string" || revision.length === 0 || revision.length > 128) return null;
  return { schema: 1, currency: "USD", unit_price_micros: unit, units, total_price_micros: total, price_revision: revision };
}
/** Integer micros to `USD d.mmmmmm` — quotient and 6-digit remainder, never
 *  floating-point rounding. */
const usdLabel = (micros: number): string =>
  `USD ${Math.floor(micros / 1_000_000)}.${String(micros % 1_000_000).padStart(6, "0")}`;
/** `run.start`: validate against the installed workflow, fetch the quotes the
 *  daemon will compare with the run's frozen ones, and name the work with the
 *  workflow's own label plus the quoted USD total (host label only — no
 *  amount or quote object ever reaches the frame). A frame-supplied
 *  `request_id` is the immutable-intent replay key; unlisted fields —
 *  actor, scope, approval, quote, token, run selectors — refuse outright. */
async function planRunStart(ctx: ActionContext, args: Record<string, unknown>): Promise<SlotPlan | ActionRefusal> {
  const keys = Object.keys(args).sort().join();
  if (!["inputs,workflow", "inputs,request_id,workflow", "inputs,selected_post_id,source_receipt_id,workflow",
       "inputs,request_id,selected_post_id,source_receipt_id,workflow"].includes(keys) ||
      typeof args.workflow !== "string" || !isObject(args.inputs)) return { code: "bad_args", text: "That request is not valid." };
  // A present request_id must meet the native identifier grammar; absent
  // keeps the host-generated id. `null` is present, not absent.
  if ("request_id" in args && (typeof args.request_id !== "string" || !RUN_REQUEST_ID.test(args.request_id)))
    return { code: "bad_args", text: "That request is not valid." };
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
  // Freeze the intent BEFORE the first quote await: workflow name, request
  // id, install/context, the label base and a copy of the slot list, so a
  // mutable installation/context mid-await cannot change what was planned.
  const workflow = flow.name;
  const labelBase = (flow.label ?? "").replace(/\s+/g, " ").trim().slice(0, LABEL_MAX) || "Start";
  const slots = [...(flow.capability_slots ?? [])];
  const request = "request_id" in args ? args.request_id as string : randomRequest();
  const installId = ctx.installId;
  const contextId = ctx.contextId;
  const expected: Record<string, CapabilityQuote["quote"]> = {};
  let totalMicros = 0;
  try {
    for (const slot of slots) {
      const quote = checkedQuote((await workspaceApps.bindingQuote(installId, slot, contextId || undefined)).quote);
      // A malformed or unsafe quote number refuses the slot itself — no
      // confirmation is drawn and no start can ever be attempted.
      if (!quote) return { code: "bad_quote", text: "That isn't available right now. Try again." };
      expected[slot] = quote;
      totalMicros += quote.total_price_micros;
      if (!Number.isSafeInteger(totalMicros)) return { code: "bad_quote", text: "That isn't available right now. Try again." };
    }
  } catch (error) { return plainRefusal(error); }
  const label = labelBase + (slots.length > 0 ? ` · ${usdLabel(totalMicros)}` : "");
  // The frozen closure carries workflow name, inputs copy, request id,
  // install/context and the primitive quoted terms. The daemon's CAS on
  // expected_quotes refuses changed intent; the host never resubmits.
  return { label, run: async (): Promise<ActionResult> => {
    let run: unknown;
    try {
      run = await workspaceApps.startRun({ install_id: installId, workflow, inputs, request_id: request,
        expected_quotes: expected, ...(contextId ? { context_id: contextId } : {}), ...source });
    } catch {
      // The start call's outcome is ambiguous — it may have landed.
      return { ok: false, refusal: UNCONFIRMED };
    }
    // A non-object reply (null, primitive) cannot confirm a started run.
    if (!isObject(run)) return { ok: false, refusal: UNCONFIRMED };
    // Only id/state cross, pinned to the request id this tap planned; the
    // closed runtime check makes a malformed reply uncertain, never a claim.
    const receipt = parseRunSlotReceipt({ run_id: run.id, request_id: request, state: run.state });
    if (!receipt) return { ok: false, refusal: UNCONFIRMED };
    ctx.onChanged();
    return { ok: true, data: receipt };
  } };
}

/** CAD-1365 — the original download slot. The host label is static; the run
 *  captures install/context and the action token before its one await, then
 *  creates a Blob and a parent-owned link only while the slot is still current. */
const ORIGINAL_EXT: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp" };
async function planOriginalDownload(get: () => ActionContext, args: Record<string, unknown>, ui?: { actionToken?: string; isCurrent?: () => boolean }): Promise<SlotPlan | ActionRefusal> {
  const checked = originalDownloadArgs(args);
  const actionToken = ui?.actionToken;
  if (!checked || typeof actionToken !== "string" || actionToken.length === 0 || actionToken.length > 256)
    return { code: "bad_args", text: "That request is not valid." };
  const assetId = checked.asset_id as string;
  const { installId, contextId, installation } = get();
  const digest = installation.digest;
  return { label: "Download original", run: async (isCurrent?: () => boolean): Promise<ActionResult> => {
    const current = () => (isCurrent ? isCurrent() : true) && (ui?.isCurrent ? ui.isCurrent() : true) &&
      get().installId === installId && get().contextId === contextId && get().installation.digest === digest;
    let image: { bytes: ArrayBuffer; mime: string };
    try {
      image = await workspaceApps.originalMedia({ action_token: actionToken, asset_id: assetId });
    } catch (error) {
      // A checked HTTP refusal is a refusal; a lost or aborted request is uncertain, never a claim.
      if (error instanceof ApiError) return { ok: false, refusal: plainRefusal(error) };
      return { ok: false, refusal: { code: "unconfirmed", text: "Cadence could not confirm this download. Check your downloads before trying again." } };
    }
    const ext = ORIGINAL_EXT[image.mime];
    if (!ext) return refuse("failed", "That didn't work. Nothing was started.");
    if (!current()) return refuse("stale", "That changed somewhere else. Reload and try again.");
    const url = URL.createObjectURL(new Blob([image.bytes], { type: image.mime }));
    try {
      const link = document.createElement("a");
      link.href = url;
      link.download = `${assetId}.${ext}`;
      document.body.appendChild(link);
      try { link.click(); } finally { link.remove(); }
    } finally {
      // Revoke after the browser has had its tick to start the download.
      setTimeout(() => URL.revokeObjectURL(url), 0);
    }
    return { ok: true, data: {} };
  } };
}

const DRAFT_ALIAS = "social.draft";

/** SOC-32: choose one local JPEG/PNG through a host-owned file control. The
 *  picker is opened synchronously from the host slot's tap; the bytes go to
 *  the board's multipart route, never through the screen port. The frame
 *  receives only the durable asset receipt and a host-minted preview ref. */
function chooseImageFile(): Promise<File | null> {
  return new Promise(resolve => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/jpeg,image/png";
    input.style.position = "fixed";
    input.style.left = "-10000px";
    input.style.top = "0";
    let settled = false;
    const finish = (file: File | null) => {
      if (settled) return;
      settled = true;
      input.remove();
      resolve(file);
    };
    input.addEventListener("change", () => finish(input.files?.[0] ?? null), { once: true });
    input.addEventListener("cancel", () => finish(null), { once: true });
    // Browsers without the `cancel` event still fire window focus when the
    // dialog closes; a file selection has already populated `files` by then.
    window.addEventListener("focus", () => { setTimeout(() => { if (!input.files?.length) finish(null); }, 0); }, { once: true });
    document.body.appendChild(input);
    input.click();
  });
}

async function planImagePick(_args: Record<string, unknown>, ui?: { actionToken?: string; allowAssetRef?: (ref: string) => void }): Promise<SlotPlan | ActionRefusal> {
  const actionToken = ui?.actionToken;
  if (typeof actionToken !== "string" || !/^[a-f0-9]{64}$/.test(actionToken) || !ui?.allowAssetRef)
    return { code: "unavailable", text: "That photo control is unavailable." };
  const allowAssetRef = ui.allowAssetRef;
  return { label: "Choose image", run: async (): Promise<ActionResult> => {
    const file = await chooseImageFile();
    if (!file) return refuse("cancelled", "Cancelled. Nothing was uploaded.");
    if (!["image/jpeg", "image/png"].includes(file.type)) return refuse("image_type", "Use a JPEG or PNG photo.");
    if (file.size > 2 * 1024 * 1024 + 64 * 1024) return refuse("image_too_large", "That photo is too large. Use one under 2 MB.");
    let uploaded: { asset_id: string; mime: string; width: number; height: number; size_bytes: number; digest: string };
    try {
      uploaded = await workspaceApps.uploadSocialImage({ action_token: actionToken, alias: DRAFT_ALIAS, file });
    } catch (error) { return { ok: false, refusal: plainRefusal(error) }; }
    const image_ref = `upload-image:${uploaded.asset_id}:${DRAFT_ALIAS}`;
    allowAssetRef(image_ref);
    return { ok: true, data: { ...uploaded, image_ref } };
  } };
}

/** The spend/publish verbs. The set is closed in `screenProtocol`. */
export function makePlanner(get: () => ActionContext): Planner {
  return (verb: SlotVerb, args, ui) => {
    if (!(SLOT_VERBS as readonly string[]).includes(verb)) return Promise.resolve({ code: "forbidden", text: "This operation requires the records service." });
    const table: Record<(typeof SLOT_VERBS)[number], () => Promise<SlotPlan | ActionRefusal>> = {
      "run.start": () => planRunStart(get(), args),
      "publish.draft.confirm": () => planConfirmPublish(get(), args, ui?.actionToken),
      "publish.draft.schedule": () => planDraftEffectChange(get(), "publish.draft.schedule", args, ui?.actionToken),
      "media.original.download": () => planOriginalDownload(get, args, ui),
      "social.image.pick": () => planImagePick(args, ui),
    };
    return table[verb as (typeof SLOT_VERBS)[number]]();
  };
}
