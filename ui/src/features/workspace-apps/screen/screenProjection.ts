import type { Installation, AppContext, WorkspaceRun, AppEffect } from "../workspaceApps";
import type { PublishIntent } from "../socialPublish";
import { INTENT_ROWS_MAX, type ScreenIntent, type ScreenIntents, type ScreenPush } from "./screenProtocol";

/** The verified `socialPublish.list` read, tagged with the scope it was
 *  issued for. A read for any other install/context is never pushed. */
export interface IntentRead {
  installId: string;
  contextId: string;
  status: "loading" | "ok" | "unavailable";
  intents: PublishIntent[];
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
  runContext: Map<string, string>): ScreenIntents {
  if (!read || read.installId !== installId || read.contextId !== contextId)
    return { status: "loading", withheld: 0, rows: [] };
  if (read.status !== "ok") return { status: read.status, withheld: 0, rows: [] };
  if (read.intents.length > INTENT_ROWS_MAX) throw new Error("The screen projection exceeds its read-only limit");
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
      destination_id: intent.destination_id, due_epoch: intent.due_epoch, timezone: intent.timezone });
  }
  return { status: read.intents.length >= INTENT_ROWS_MAX ? "truncated" : "ok",
    withheld: read.intents.length - rows.length, rows };
}

const text = (value: string, limit = 512) => value.slice(0, limit);
export function screenTag(installation: Installation): string | null {
  if (!installation.approved || !installation.executable) return null;
  const tags = installation.files.map(p => /^screens\/([a-z0-9][a-z0-9-]{0,31})\/screens\.json$/.exec(p)?.[1]).filter((v): v is string => !!v);
  // The first portable-screen MVP mounts one unambiguous declared screen.
  return tags.length === 1 ? tags[0] : null;
}
export function screenProjection(installation: Installation, tag: string, contextId: string,
  contexts: AppContext[], runs: WorkspaceRun[], effects: AppEffect[], intents?: IntentRead): ScreenPush {
  const active = contexts.filter(c => c.install_id === installation.install_id && c.state === "active");
  if (contextId && !active.some(c => c.id === contextId)) throw new Error("The screen context is unavailable");
  const scopedRuns = runs.filter(r => r.install_id === installation.install_id && (!contextId || r.context_id === contextId));
  const scopedIds = new Set(scopedRuns.map(r => r.id));
  const scopedEffects = effects.filter(e => e.authority.install_id === installation.install_id && scopedIds.has(e.authority.run_id) && (!contextId || e.authority.context?.id === contextId));
  if (active.length > 128 || scopedRuns.length > 256 || scopedEffects.length > 256) throw new Error("The screen projection exceeds its read-only limit");
  const projection: ScreenPush = {
    v: 1, op: "screen", install_id: installation.install_id, digest: installation.digest,
    tag, context_id: contextId,
    installation: { install_id: installation.install_id, name: text(installation.name, 128),
      title: text(installation.title), version: text(installation.version, 128),
      digest: installation.digest, summary: text(installation.summary, 4096) },
    contexts: active.map(c => ({ id: c.id, label: text(c.config.label) })),
    runs: scopedRuns.map(r => ({ id: r.id, state: text(r.state, 64),
      title: text(r.snapshot.workflow.title), snapshot_digest: r.snapshot_digest,
      workflow: { title: text(r.snapshot.workflow.title) }, context_id: r.context_id ?? "" })),
    outbox: scopedEffects.map(e => ({ effect_id: e.effect_id, state: text(e.state, 64), title: text(e.record.title ?? ""),
      run_id: e.authority.run_id, context_id: e.authority.context?.id ?? "" })),
    publish_intents: scopedIntents(intents, installation.install_id, contextId,
      new Map(scopedRuns.map(r => [r.id, r.context_id ?? ""]))),
    now: Math.floor(Date.now() / 1000),
  };
  if (new TextEncoder().encode(JSON.stringify(projection)).byteLength > 128 * 1024) throw new Error("The screen projection exceeds its message limit");
  return projection;
}
