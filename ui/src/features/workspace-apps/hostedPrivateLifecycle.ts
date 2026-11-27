import { ApiError } from "../../lib/api";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { appExplorer, publishHostedLifecycle, workspaceApps, type Installation } from "./workspaceApps";

export type HostedLifecycleAction = "remove" | "restore";
type HostedLifecycleResult =
  | { kind: "confirmed"; installation: Installation; restoreDigest?: string }
  | { kind: "uncertain" }
  | { kind: "denied"; error: unknown };
export type HostedLifecycleOutcome =
  | { kind: "confirmed"; installation: Installation; requestedStateReached: boolean; message: string }
  | { kind: "uncertain"; message: string }
  | { kind: "denied"; expired: boolean; message: string };
type Attempt = { action: HostedLifecycleAction; busy: boolean; sent: boolean; restoreDigest?: string };
const attempts = new Map<string, Attempt>();
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
    if (sessionCurrent(generation, epoch) && validRow(row, id) && typeof row.digest === "string" && row.digest.length > 0) {
      return { kind: "confirmed", installation: row };
    }
  } catch {
    // A failed read cannot settle an ambiguous mutation.
  }
  return { kind: "uncertain" };
}

/** Read independently after an ambiguous write; never replay the write. */
async function checkHostedLifecycle(id: string, generation: number): Promise<HostedLifecycleResult> {
  const attempt = attempts.get(id);
  if (attempt?.busy) return { kind: "uncertain" };
  if (attempt) attempt.busy = true;
  try {
    const result = await readLifecycle(id, generation, admissionEpoch);
    if (result.kind === "confirmed") {
      result.restoreDigest = attempt?.restoreDigest;
      if (attempts.get(id) === attempt) attempts.delete(id);
    }
    return result;
  } finally {
    if (attempt) attempt.busy = false;
  }
}

/** Validate the fresh source and deadline, then one write and its receipt. */
async function changeHostedLifecycle(id: string, action: HostedLifecycleAction, generation: number): Promise<HostedLifecycleResult> {
  const epoch = admissionEpoch;
  if (!sessionCurrent(generation, epoch) || attempts.has(id)) return { kind: "uncertain" };
  const attempt: Attempt = { action, busy: true, sent: false };
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
    if (action === "restore") attempt.restoreDigest = row.digest;
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
          return { kind: "confirmed", installation: receipt.installation, restoreDigest: row.digest };
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
    if (result.kind === "confirmed") {
      result.restoreDigest = attempt.restoreDigest;
      settled = true;
    }
    return result;
  } finally {
    attempt.busy = false;
    if ((!attempt.sent || settled) && attempts.get(id) === attempt) attempts.delete(id);
  }
}

/** Run one command or readback and publish only a current, confirmed result. */
export async function runHostedLifecycle(id: string, action: HostedLifecycleAction, checkOnly: boolean): Promise<HostedLifecycleOutcome | null> {
  const generation = privateSessionGeneration();
  const epoch = admissionEpoch;
  const result = await (checkOnly ? checkHostedLifecycle(id, generation) : changeHostedLifecycle(id, action, generation));
  if (!sessionCurrent(generation, epoch)) return null;
  if (result.kind === "uncertain") {
    return { kind: "uncertain", message: "The result could not be confirmed. Check again before making another change." };
  }
  if (result.kind === "denied") {
    const expired = result.error instanceof ApiError && result.error.code === "restore_window";
    const message = expired ? "The restore window has closed."
      : result.error instanceof ApiError && (result.error.status === 401 || result.error.status === 403)
        ? "The app change was denied. Check your workspace access."
        : "The app change was not allowed or could not be verified.";
    return { kind: "denied", expired, message };
  }
  const requestedStateReached = action === "remove"
    ? result.installation.state === "removed"
    : result.installation.state === "active" && result.installation.digest === result.restoreDigest;
  const message = !requestedStateReached ? "The requested change was not confirmed. The app's current state is shown below."
    : action === "remove" ? "Removed — its data is retained."
    : "Restored — connection grants must be set up again.";
  publishHostedLifecycle(result.installation);
  return { kind: "confirmed", installation: result.installation, requestedStateReached, message };
}
