import type { Installation, AppContext, WorkspaceRun, AppEffect, AppBinding, CapabilityQuote, SourceReceipt } from "../workspaceApps";
import { PUBLISH_LIST_CAP, type PublishIntent } from "../socialPublish";
import { shapeFor, type ScreenDefaults, type ScreenIntent, type ScreenIntents, type ScreenPrice, type ScreenPush,
  type ScreenRefusal, type ScreenRunV2, type ScreenSourcePost, type ScreenSources } from "./screenProtocol";

/** The verified `socialPublish.list` read, tagged with the scope it was
 *  issued for. A read for any other install/context is never pushed. */
export interface IntentRead {
  installId: string;
  contextId: string;
  status: "ok" | "unavailable";
  intents: PublishIntent[];
}
/** The next read for a scope. A failed read keeps the last good read of the
 *  SAME scope (a transient error must not blank the calendar); any other
 *  scope, or no earlier good read, becomes `unavailable`. */
export function settleIntentRead(previous: IntentRead | undefined, installId: string, contextId: string,
  intents: PublishIntent[] | null): IntentRead {
  if (intents) return { installId, contextId, status: "ok", intents };
  if (previous?.status === "ok" && previous.installId === installId && previous.contextId === contextId) return previous;
  return { installId, contextId, status: "unavailable", intents: [] };
}
const ID_MAX = 128;
const isId = (value: string) => value.length > 0 && value.length <= ID_MAX;
function isZone(timezone: string): boolean {
  if (timezone.length === 0 || timezone.length > 64) return false;
  try { new Intl.DateTimeFormat("en-CA", { timeZone: timezone }); return true; } catch { return false; }
}
/** Same-scope rows only, joined to an in-scope run under its exact context.
 *  Foreign, dangling, malformed and duplicated-id rows are withheld (counted,
 *  never sent). Secrets in the intent (grant, approval, key, receipt) are
 *  never copied. */
function scopedIntents(read: IntentRead | undefined, installId: string, contextId: string,
  runContext: Map<string, string>, v2: boolean): ScreenIntents {
  if (!read || read.installId !== installId || read.contextId !== contextId)
    return { status: "loading", withheld: 0, rows: [] };
  if (read.status !== "ok") return { status: read.status, withheld: 0, rows: [] };
  if (read.intents.length > PUBLISH_LIST_CAP) throw new Error("The screen projection exceeds its read-only limit");
  const uses = new Map<string, number>();
  for (const intent of read.intents) uses.set(intent.intent_id, (uses.get(intent.intent_id) ?? 0) + 1);
  const rows: ScreenIntent[] = [];
  for (const intent of read.intents) {
    const context = intent.context_id ?? "";
    // runContext holds only in-scope runs, so this join also enforces the selected context.
    if (uses.get(intent.intent_id) !== 1 || intent.install_id !== installId ||
        runContext.get(intent.run_id) !== context ||
        ![intent.intent_id, intent.run_id, intent.effect_id, intent.destination_id].every(isId) ||
        !Number.isSafeInteger(intent.due_epoch) || intent.due_epoch < 0 || !isZone(intent.timezone)) continue;
    rows.push({ intent_id: intent.intent_id, install_id: installId, context_id: context,
      run_id: intent.run_id, effect_id: intent.effect_id, state: intent.state, channel: intent.channel,
      destination_id: intent.destination_id, due_epoch: intent.due_epoch, timezone: intent.timezone,
      ...(v2 ? present({ refusal: refusalOf(intent.refusal?.code ?? "", intent.refusal?.message ?? ""),
        permalink: instagramLink(intent.permalink) }) : {}) });
  }
  return { status: read.intents.length >= PUBLISH_LIST_CAP ? "truncated" : "ok",
    withheld: read.intents.length - rows.length, rows };
}

const text = (value: string, limit = 512) => value.slice(0, limit);

/** CAD-1123 HP1 — the board-held reads the v2 projection adds, each tagged
 *  with the scope it was issued for. A read for another scope is ignored. */
export interface ScreenExtras {
  installId: string;
  contextId: string;
  /** Configured bindings of this install (any context; filtered here). */
  bindings: AppBinding[];
  /** Registered enabled workers; `null` while unknown. */
  workers: number | null;
  /** Current quotes by slot for this scope. */
  quotes: Record<string, CapabilityQuote["quote"]>;
  /** The newest finished read-slot run's results; `undefined` while loading,
   *  `null` when the read failed. */
  source?: { runId: string; receipts: SourceReceipt[] } | null;
  /** Reviewer-approved text by artifact id (only artifacts of pushed runs are used). */
  texts: Map<string, string>;
}
const EXCERPT_MAX = 600;
const RATIONALE_MAX = 280;
const POSTS_MAX = 24;
const STEPS_MAX = 16;
const KEYS_MAX = 16;
const SLOTS_MAX = 8;
/** The wire grammars the package adapter (screen-v2.mjs) checks. */
const CODE = /^[a-z0-9][a-z0-9_.-]{0,63}$/;
const KEY = /^[a-z_][a-z0-9_]{0,63}$/;
const SLOT = /^[a-z][a-z0-9_.-]{0,63}$/;
const HANDLE = /^@?[A-Za-z0-9._]{1,30}$/;
const PHASE_KIND = /^[a-z0-9][a-z0-9._-]{0,63}$/;
/** Drop the keys whose value is `null`/`undefined` — the v2 wire omits
 *  what the host cannot state rather than sending a placeholder. */
function present<T extends Record<string, unknown>>(row: T): { [K in keyof T]?: Exclude<T[K], null | undefined> } {
  return Object.fromEntries(Object.entries(row).filter(([, value]) => value !== null && value !== undefined)) as never;
}
function refusalOf(code: string, message: string): ScreenRefusal | null {
  const line = message.replace(/\s+/g, " ").trim();
  if (!line) return null;
  return { code: CODE.test(code) ? code : "refused", text: text(line, RATIONALE_MAX) };
}
function httpsOn(value: string | null | undefined, ok: (host: string) => boolean, max = 2048): string | null {
  if (typeof value !== "string" || value.length > max) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.port && ok(url.hostname) &&
      url.href.length <= max ? url.href : null;
  } catch { return null; }
}
/** A posted or source permalink: https on instagram.com only. */
export const instagramLink = (value: string | null | undefined) =>
  httpsOn(value, host => host === "instagram.com" || host === "www.instagram.com", 512);
/** A source thumbnail: https on the Instagram CDN hosts the frame CSP can admit. */
export const instagramThumb = (value: string | null | undefined) =>
  httpsOn(value, host => host.endsWith(".cdninstagram.com") || host.endsWith(".fbcdn.net"));
/** Integer micros → the shortest exact decimal string ("60000" → "0.06"). */
function decimal(micros: number): string {
  const whole = Math.floor(micros / 1_000_000);
  const frac = String(micros % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return frac ? `${whole}.${frac}` : String(whole);
}
function priceOf(quote: CapabilityQuote["quote"] | undefined): ScreenPrice | null {
  if (!quote || quote.schema !== 1 || typeof quote.currency !== "string" || !/^[A-Z]{3}$/.test(quote.currency) ||
      !Number.isSafeInteger(quote.total_price_micros) || quote.total_price_micros < 0 ||
      quote.total_price_micros >= 1e15) return null;
  return { amount: decimal(quote.total_price_micros), currency: quote.currency };
}
function prices(quotes: Record<string, CapabilityQuote["quote"]> | undefined, slots?: Set<string>): Record<string, ScreenPrice> {
  const out: Record<string, ScreenPrice> = {};
  for (const [slot, quote] of Object.entries(quotes ?? {})) {
    if (Object.keys(out).length >= SLOTS_MAX || !SLOT.test(slot) || (slots && !slots.has(slot))) continue;
    const price = priceOf(quote);
    if (price) out[slot] = price;
  }
  return out;
}
/** The input names an installed workflow marks `context_default`. */
function contextDefaultKeys(installation: Installation, workflow?: string): Map<string, string | null> {
  const keys = new Map<string, string | null>();
  for (const flow of installation.workflows ?? []) {
    if (workflow !== undefined && flow.name !== workflow) continue;
    for (const input of flow.inputs ?? []) {
      if (input.context_default && KEY.test(input.name) && !keys.has(input.name) && keys.size < KEYS_MAX)
        keys.set(input.name, typeof input.default === "string" ? input.default : null);
    }
  }
  return keys;
}
/** The daemon always sends these lists; a partial row degrades to empty. */
const list = <T>(value: T[] | undefined | null): T[] => Array.isArray(value) ? value : [];
function phaseOf(run: WorkspaceRun): string {
  switch (run.state) {
    case "awaiting_approval": case "approved": return "awaiting";
    case "succeeded": return "ready";
    case "failed": return "failed";
    case "cancelled": return "cancelled";
  }
  const kinds = new Map(list(run.snapshot.workflow.steps).map(step => [step.id, step.kind]));
  const active = list(run.steps).find(step => step.state === "dispatched") ?? list(run.steps).find(step => step.state === "pending");
  const kind = active ? kinds.get(active.step_id) ?? "" : "";
  if (kind.startsWith("review")) return "checking";
  return PHASE_KIND.test(kind) ? `working:${kind}` : "working:step";
}
const epoch = (value: number | undefined) => Number.isSafeInteger(value) && value! >= 0 ? value! : null;
const TERMINAL = new Set(["succeeded", "failed", "cancelled"]);
function runV2(run: WorkspaceRun, installation: Installation, extras: ScreenExtras | undefined) {
  const flow = (installation.workflows ?? []).find(value =>
    !!value.source_digest && value.source_digest === run.snapshot.workflow.source_digest);
  const slots = Object.keys(run.snapshot.capabilities ?? {});
  const effects = slots.map(slot => installation.capabilities?.[slot]?.effect);
  const steps = list(run.snapshot.workflow.steps);
  const receipts = list(run.steps);
  const reviews = list(run.reviews);
  const kinds = new Map(steps.map(step => [step.id, step.kind]));
  const approve = reviews.find(review => review.decision === "approve");
  const lastReview = reviews[reviews.length - 1];
  const artifact = approve ? list(run.artifacts).find(value => value.digest === approve.artifact_digest) : undefined;
  const excerpt = artifact ? extras?.texts.get(artifact.id) : undefined;
  const used: NonNullable<ScreenRunV2["inputs_used"]> = {};
  if (flow) for (const key of contextDefaultKeys(installation, flow.name).keys()) {
    const value = run.snapshot.inputs?.[key];
    if (typeof value === "string")
      used[key] = { value: text(value), origin: text(run.snapshot.input_origins?.[key] ?? "run_override", 32) };
  }
  const failedStep = receipts.find(step => step.state === "failed");
  const refusal: ScreenRefusal | null = run.state === "failed"
    ? (lastReview?.decision === "revise" ? refusalOf("review_revise", lastReview.rationale) : null) ??
      { code: "step_failed", text: failedStep ? `Step ${text(failedStep.step_id, 32)} (${text(kinds.get(failedStep.step_id) ?? "step", 64)}) did not finish.` : "The run did not finish." }
    : run.state === "cancelled" ? { code: "cancelled", text: "The run was cancelled." } : null;
  const runPrice = prices(run.snapshot.quotes);
  const postId = run.snapshot.source?.post?.id;
  const workflow = present({ name: flow && SLOT.test(flow.name) ? flow.name : null,
    kind: slots.length > 0 && effects.every(effect => effect === "read") ? "read" : "draft" });
  const fields = present({
    created: epoch(run.created),
    closed: TERMINAL.has(run.state) ? epoch(run.updated) : null,
    phase: phaseOf(run),
    steps: steps.slice(0, STEPS_MAX).map(step => ({ id: text(step.id, 64),
      kind: text(step.kind, 64), state: text(receipts.find(value => value.step_id === step.id)?.state ?? "pending", 32) })),
    inputs_used: Object.keys(used).length ? used : null,
    caption_excerpt: typeof excerpt === "string" ? text(excerpt, EXCERPT_MAX) : null,
    artifact_id: artifact && isId(artifact.id) ? artifact.id : null,
    review: lastReview ? { decision: text(lastReview.decision, 32), rationale: text(lastReview.rationale, RATIONALE_MAX) } : null,
    price: Object.keys(runPrice).length ? runPrice : null,
    approved: run.approval && typeof run.approval.by === "string" && epoch(run.approval.at) !== null
      ? { by_display: run.approval.by === "operator" ? "Operator" : text(run.approval.by, 64), at: run.approval.at } : null,
    source_post_id: typeof postId === "string" && isId(postId) ? postId : null,
    refusal,
    image_ref: approve?.asset_receipt_id && /^[A-Za-z0-9_-]{1,120}$/.test(run.id) ? `image:${run.id}` : null,
  });
  return { workflow, fields };
}
/** The newest finished read-slot run in scope — whose results are the library. */
export function latestSourceRun(installation: Installation, runs: WorkspaceRun[]): WorkspaceRun | null {
  const reads = runs.filter(run => run.state === "succeeded" && Object.keys(run.snapshot.capabilities ?? {}).some(slot =>
    installation.capabilities?.[slot]?.effect === "read"));
  return reads.reduce<WorkspaceRun | null>((newest, run) => !newest || (run.created ?? 0) >= (newest.created ?? 0) ? run : newest, null);
}
/** The library, or `null` (omitted: the frame shows it as unavailable)
 *  while no verified read of the newest read-slot run is held. */
function sourcesOf(installation: Installation, scopedRuns: WorkspaceRun[], extras: ScreenExtras | undefined): ScreenSources | null {
  const latest = latestSourceRun(installation, scopedRuns);
  if (!latest || !extras?.source || extras.source.runId !== latest.id) return null;
  const receipt = extras.source.receipts.find(value => value.run_id === latest.id && value.result?.kind === "social.source.posts");
  if (!receipt || typeof receipt.result.handle !== "string" || !HANDLE.test(receipt.result.handle)) return null;
  const seen = new Set<string>();
  const posts: ScreenSourcePost[] = [];
  for (const post of list(receipt.result.posts)) {
    const permalink = instagramLink(post.permalink);
    const published = epoch(post.published_at_unix ?? undefined);
    // A post the frame could not show honestly (no link, no time) is withheld.
    if (posts.length >= POSTS_MAX || typeof post.id !== "string" || !isId(post.id) || seen.has(post.id) ||
        !permalink || published === null) continue;
    seen.add(post.id);
    posts.push(present({ id: post.id, caption: text(post.caption ?? "", EXCERPT_MAX), published_at: published,
      permalink, media_kind: text(post.media_kind ?? "", 32), thumb_url: instagramThumb(post.preview_url) }) as ScreenSourcePost);
  }
  return present({ handle: receipt.result.handle, run_id: latest.id, fetched_at: epoch(latest.updated), posts }) as ScreenSources;
}
function defaultsOf(installation: Installation, context: AppContext | undefined): ScreenDefaults {
  const out: ScreenDefaults = { context_id: context?.id ?? "", revision: context?.revision ?? 0, values: {} };
  for (const [key, appDefault] of contextDefaultKeys(installation)) {
    const saved = context?.config.input_defaults?.[key];
    const value = typeof saved === "string" ? saved : appDefault;
    // Values are never truncated (a later save would write the cut text).
    if (typeof value === "string" && value.length <= 512) out.values[key] = value;
  }
  return out;
}
function readinessOf(installation: Installation, contextId: string, extras: ScreenExtras | undefined): { ok: boolean; blockers: string[] } {
  const blockers: string[] = [];
  if (!installation.approved) blockers.push("install_unapproved");
  for (const slot of Object.keys(installation.capabilities ?? {})) {
    const bound = extras?.bindings.some(b => b.slot === slot && (b.context_id ?? "") === contextId &&
      b.state === "configured" && b.config.bundle_digest === installation.digest);
    if (!bound && SLOT.test(slot) && slot.length <= 50) blockers.push(`slot_unbound.${slot}`);
  }
  if (extras?.workers === 0) blockers.push("no_workers");
  const kept = blockers.slice(0, KEYS_MAX);
  return { ok: kept.length === 0, blockers: kept };
}

export function screenTag(installation: Installation): string | null {
  if (!installation.approved || !installation.executable) return null;
  const tags = installation.files.map(p => /^screens\/([a-z0-9][a-z0-9-]{0,31})\/screens\.json$/.exec(p)?.[1]).filter((v): v is string => !!v);
  // The first portable-screen MVP mounts one unambiguous declared screen.
  return tags.length === 1 ? tags[0] : null;
}
export function screenProjection(installation: Installation, tag: string, contextId: string,
  contexts: AppContext[], runs: WorkspaceRun[], effects: AppEffect[], intents?: IntentRead, extras?: ScreenExtras): ScreenPush {
  const active = contexts.filter(c => c.install_id === installation.install_id && c.state === "active");
  if (contextId && !active.some(c => c.id === contextId)) throw new Error("The screen context is unavailable");
  const scopedRuns = runs.filter(r => r.install_id === installation.install_id && (!contextId || r.context_id === contextId));
  const scopedIds = new Set(scopedRuns.map(r => r.id));
  const scopedEffects = effects.filter(e => e.authority.install_id === installation.install_id && scopedIds.has(e.authority.run_id) && (!contextId || e.authority.context?.id === contextId));
  if (active.length > 128 || scopedRuns.length > 256 || scopedEffects.length > 256) throw new Error("The screen projection exceeds its read-only limit");
  // Board reads tagged for another install or context are never used.
  const scoped = extras && extras.installId === installation.install_id && extras.contextId === contextId ? extras : undefined;
  const projection: ScreenPush = {
    v: 1, op: "screen", install_id: installation.install_id, digest: installation.digest,
    tag, context_id: contextId,
    installation: { install_id: installation.install_id, name: text(installation.name, 128),
      title: text(installation.title), version: text(installation.version, 128),
      digest: installation.digest, summary: text(installation.summary, 4096) },
    contexts: active.map(c => ({ id: c.id, label: text(c.config.label) })),
    runs: scopedRuns.map(r => {
      const v2 = runV2(r, installation, scoped);
      return { id: r.id, state: text(r.state, 64),
        title: text(r.snapshot.workflow.title), snapshot_digest: r.snapshot_digest,
        workflow: { title: text(r.snapshot.workflow.title), ...v2.workflow }, context_id: r.context_id ?? "", ...v2.fields };
    }),
    outbox: scopedEffects.map(e => ({ effect_id: e.effect_id, state: text(e.state, 64), title: text(e.record.title ?? ""),
      run_id: e.authority.run_id, context_id: e.authority.context?.id ?? "" })),
    publish_intents: scopedIntents(intents, installation.install_id, contextId,
      new Map(scopedRuns.map(r => [r.id, r.context_id ?? ""])), true),
    ...present({ sources: sourcesOf(installation, scopedRuns, scoped) }),
    defaults: defaultsOf(installation, active.find(c => c.id === contextId)),
    prices: prices(scoped?.quotes, new Set(Object.entries(installation.capabilities ?? {})
      .filter(([, slot]) => slot.effect === "read" || slot.effect === "draft").map(([name]) => name))),
    readiness: readinessOf(installation, contextId, scoped),
    actions: [],
    now: Math.floor(Date.now() / 1000),
  };
  // The base CAD-1006 shape must fit; each child's own shape is re-checked at send.
  if (!shapeFor(projection, false)) throw new Error("The screen projection exceeds its message limit");
  return projection;
}
