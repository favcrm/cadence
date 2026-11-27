import { ApiError } from "../../lib/api";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { appExplorer, workspaceApps, type Installation } from "./workspaceApps";

export type HostedLifecycleAction = "remove" | "restore";
export type HostedLifecycleResult =
  | { kind: "confirmed"; installation: Installation }
  | { kind: "uncertain" }
  | { kind: "denied"; error: unknown };
const attempts = new Map<string, { action: HostedLifecycleAction; busy: boolean; sent: boolean }>();
let admissionEpoch = 0;
onPrivateReadsPaused(() => { admissionEpoch += 1; });
onPrivateSessionReset(() => { attempts.clear(); });

export function pendingHostedLifecycle(id: string): HostedLifecycleAction | null {
  const attempt = attempts.get(id);
  return attempt?.sent ? attempt.action : null;
}

export function hostedRestoreAvailable(deadline: unknown): boolean {
  return typeof deadline === "number" && Number.isSafeInteger(deadline) && deadline > 0 && Date.now() <= deadline;
}

function validRow(value: unknown, id: string): value is Installation {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const row = value as Installation;
  if (row.install_id !== id || row.source_kind !== "private") return false;
  if (row.state === "removed") return row.executable === false &&
    Number.isSafeInteger(row.removed_at) && (row.removed_at as number) > 0 &&
    Number.isSafeInteger(row.restore_until) && (row.restore_until as number) - (row.removed_at as number) === 30 * 24 * 60 * 60 * 1000;
  return (row.state === "active" || row.state === "disabled") && typeof row.executable === "boolean" &&
    (row.state !== "disabled" || !row.executable) &&
    (row.state !== "active" || typeof row.approved !== "boolean" || row.executable === row.approved) &&
    row.removed_at === null && row.restore_until === null && typeof row.digest === "string" && row.digest.length > 0;
}

function sessionCurrent(generation: number, epoch: number): boolean {
  return privateReadsAllowed() && privateSessionGeneration() === generation && admissionEpoch === epoch;
}

async function readLifecycle(id: string, generation: number, epoch: number): Promise<HostedLifecycleResult> {
  if (!sessionCurrent(generation, epoch)) return { kind: "uncertain" };
  try {
    const row = await workspaceApps.detail(id);
    if (sessionCurrent(generation, epoch) && validRow(row, id)) return { kind: "confirmed", installation: row };
  } catch {
    // A failed read cannot settle an ambiguous mutation.
  }
  return { kind: "uncertain" };
}

/** Read independently after an ambiguous write; never replay the write. */
export async function checkHostedLifecycle(id: string, generation: number): Promise<HostedLifecycleResult> {
  const attempt = attempts.get(id);
  if (attempt?.busy) return { kind: "uncertain" };
  if (attempt) attempt.busy = true;
  try {
    const result = await readLifecycle(id, generation, admissionEpoch);
    if (result.kind === "confirmed" && attempts.get(id) === attempt) attempts.delete(id);
    return result;
  } finally {
    if (attempt) attempt.busy = false;
  }
}

/** Validate the fresh source and deadline, then one write and its receipt. */
export async function changeHostedLifecycle(id: string, action: HostedLifecycleAction, generation: number): Promise<HostedLifecycleResult> {
  const epoch = admissionEpoch;
  if (!sessionCurrent(generation, epoch) || attempts.has(id)) return { kind: "uncertain" };
  const attempt = { action, busy: true, sent: false };
  attempts.set(id, attempt);
  let settled = false;
  try {
    const before = await readLifecycle(id, generation, epoch);
    if (!sessionCurrent(generation, epoch)) return { kind: "uncertain" };
    if (before.kind !== "confirmed") return { kind: "denied", error: new Error("The app's current state could not be verified.") };
    const row = before.installation;
    if (action === "restore" && row.state !== "removed") return before;
    if (action === "restore" && !hostedRestoreAvailable(row.restore_until)) {
      return { kind: "denied", error: new ApiError("The restore window has closed.", 409, { code: "restore_window" }) };
    }
    if (action === "remove" && row.state === "removed") return before;
    attempt.sent = true;
    try {
      const response = action === "remove" ? await appExplorer.hostedRemove(id) : await appExplorer.hostedRestore(id);
      if (!sessionCurrent(generation, epoch)) return { kind: "uncertain" };
      if (action === "remove" && validRow(response, id) && response.state === "removed") {
        settled = true;
        return { kind: "confirmed", installation: response };
      }
      if (action === "restore" && response && typeof response === "object" && !Array.isArray(response)) {
        const receipt = response as { restored?: unknown; digest?: unknown; installation?: unknown };
        if (receipt.restored === true && validRow(receipt.installation, id) && receipt.installation.state === "active" &&
          receipt.digest === receipt.installation.digest && receipt.digest === row.digest) {
          settled = true;
          return { kind: "confirmed", installation: receipt.installation };
        }
      }
    } catch (error) {
      if (!sessionCurrent(generation, epoch)) return { kind: "uncertain" };
      if (error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 408 && error.status !== 429) {
        settled = true;
        return { kind: "denied", error };
      }
    }
    const result = await readLifecycle(id, generation, epoch);
    settled = result.kind === "confirmed";
    return result;
  } finally {
    attempt.busy = false;
    if ((!attempt.sent || settled) && attempts.get(id) === attempt) attempts.delete(id);
  }
}
