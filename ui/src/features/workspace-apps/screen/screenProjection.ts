import type { Installation, AppContext, WorkspaceRun, AppEffect } from "../workspaceApps";
import type { ScreenPush } from "./screenProtocol";

const text = (value: string, limit = 512) => value.slice(0, limit);
export function screenTag(installation: Installation): string | null {
  if (!installation.approved || !installation.executable) return null;
  const tags = installation.files.map(p => /^screens\/([a-z0-9][a-z0-9-]{0,31})\/screens\.json$/.exec(p)?.[1]).filter((v): v is string => !!v);
  // The first portable-screen MVP mounts one unambiguous declared screen.
  return tags.length === 1 ? tags[0] : null;
}
export function screenProjection(installation: Installation, tag: string, contextId: string,
  contexts: AppContext[], runs: WorkspaceRun[], effects: AppEffect[]): ScreenPush {
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
      workflow: { title: text(r.snapshot.workflow.title) } })),
    outbox: scopedEffects.map(e => ({ effect_id: e.effect_id, state: text(e.state, 64), title: text(e.record.title ?? "") })),
    now: Math.floor(Date.now() / 1000),
  };
  if (new TextEncoder().encode(JSON.stringify(projection)).byteLength > 128 * 1024) throw new Error("The screen projection exceeds its message limit");
  return projection;
}
