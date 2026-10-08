/**
 * CAD-1168: the conversation-owned composer state — the typed draft and
 * the attachment queue — held OUTSIDE the component.
 *
 * The defect this repairs: the composer's upload promises used to update
 * component `useState`, so switching conversations unmounted the pane and
 * the completion landed nowhere — switching back restored an `uploading`
 * row forever. Here the queue is keyed by the conversation (or `home`)
 * and lives at module scope, so:
 *
 * - an upload keeps running while the pane is unmounted and lands on its
 *   own row when the pane returns;
 * - a row's `generation` is minted per queue, so a remove/re-pick (or a
 *   restored row) can never be patched by a stale completion;
 * - row keys are minted per queue too, so a restored queue's keys never
 *   collide with rows added after the remount;
 * - removing a row ends its life: a late completion finds no row and
 *   changes nothing.
 *
 * Frame ownership (CAD-1168): a displayed installation/context/absence
 * frame owns its own LIVE unsent intent slot even when the underlying
 * native conversation is the same. Entering a different registered frame
 * parks the whole bundle (text, refs, files, captured origin and
 * reservations) under its original owner and exposes a fresh slot for
 * the new frame; the parked bundle stays explicitly reachable through
 * saved-draft recovery. A transient unavailable frame (still loading, or
 * failed) is never registered as a captured absence.
 *
 * Pure state + network; no React import, so the rules are testable in a
 * plain Node test.
 */
import { api, ApiError, type ChatFileDestination, type ChatFileRow } from "../../../lib/api";
import type { ThreadRef } from "../../../lib/types";
import { attachFits, attachRefusal, sameAttachDestination, type AttachDestination, type AttachItem } from "./attach";

const EMPTY: AttachItem[] = [];
const EMPTY_REFS: ThreadRef[] = [];
const EMPTY_SAVED: SavedIntent[] = [];

/** The keyed draft cell: text, refs, an edit version and the one
 *  in-flight handoff reservation. The Composer subscribes to it, so the
 *  visible box and the stored value are the same state after a remount
 *  or a settlement — there is no second local copy to diverge. */
export interface ComposerDraft {
  text: string;
  refs: ThreadRef[];
  /** Bumped on every explicit edit; a settlement clears only its own. */
  version: number;
  /** True while an original envelope from this key awaits settlement. */
  reserved: boolean;
}

const EMPTY_DRAFT: ComposerDraft = { text: "", refs: EMPTY_REFS, version: 0, reserved: false };

/** The app binding an intent was created under. `context_id` absent is
 *  the same explicit absence a send carried — never filled in later. */
export interface ComposerScope {
  install_id: string;
  context_id?: string;
}

/** A displayed frame's identity: `?` before any frame is known, `-` for
 *  a genuinely registered absence, or the install+context pair. */
function frameKey(frame: ComposerScope | null | undefined): string {
  if (frame === undefined) return "?";
  if (frame === null) return "-";
  return `${frame.install_id}|${frame.context_id ?? ""}`;
}

interface DraftCell {
  /** Stable slot id — survives a family move and names one bundle for
   *  saved-draft recovery. */
  id: number;
  /** The conversation/subject family this slot currently belongs to. */
  family: string;
  /** The pane's registered frame for this slot: undefined when no frame
   *  is known yet, null for a registered absence. */
  frame?: ComposerScope | null;
  text: string;
  refs: ThreadRef[];
  version: number;
  /** The version one in-flight handoff reserved, or null. */
  reserved: number | null;
  /** The binding captured at this intent's first edit: undefined when no
   *  origin was captured, null when the origin was an explicit absence.
   *  A later frame never retargets it. */
  binding?: ComposerScope | null;
  snapshot: ComposerDraft;
}

interface Family {
  key: string;
  slots: DraftCell[];
  activeId: number;
  /** Bundles parked under this family, reachable by explicit restore. */
  saved: SavedIntent[];
  savedSnapshot: SavedIntent[];
  listeners: Set<() => void>;
}

interface Queue {
  items: AttachItem[];
}

const families = new Map<string, Family>();
const slotsById = new Map<number, DraftCell>();
const queues = new Map<number, Queue>();
/** Row keys and upload generations are minted module-wide, never per
 *  queue: a row's identity survives a scope/frame move, and a queue that
 *  was disposed and recreated can never re-mint an old row's key or
 *  generation. */
let nextItemKey = 0;
let nextGeneration = 0;
let nextSlotId = 0;
/** The in-flight transport per row — remove cancels only its own. */
const controllers = new Map<string, AbortController>();
const appliedSeeds = new Map<string, object>();
const savedListeners = new Set<() => void>();

/** A parent-held seed object is a one-shot intent, not a reset on every
 *  remount. A new explicit seed object can still replace this draft. */
export function consumeSeed(key: string, signal: object): boolean {
  if (appliedSeeds.get(key) === signal) return false;
  appliedSeeds.set(key, signal);
  return true;
}

function newCell(family: Family, frame: ComposerScope | null | undefined): DraftCell {
  const cell: DraftCell = {
    id: ++nextSlotId,
    family: family.key,
    frame,
    text: "",
    refs: EMPTY_REFS,
    version: 0,
    reserved: null,
    binding: undefined,
    snapshot: EMPTY_DRAFT,
  };
  family.slots.push(cell);
  slotsById.set(cell.id, cell);
  return cell;
}

function familyFor(key: string): Family {
  const existing = families.get(key);
  if (existing) return existing;
  const family: Family = {
    key,
    slots: [],
    activeId: 0,
    saved: [],
    savedSnapshot: [],
    listeners: new Set(),
  };
  families.set(key, family);
  const cell = newCell(family, undefined);
  family.activeId = cell.id;
  return family;
}

function activeOf(family: Family): DraftCell {
  return slotsById.get(family.activeId)!;
}

/** Whether a cell holds any unsent intent: text, refs, a held handoff or
 *  queued rows. The whole bundle is the unit — halves never move apart. */
function hasIntent(cell: DraftCell): boolean {
  if (cell.text !== "" || cell.refs.length > 0 || cell.reserved !== null) return true;
  const q = queues.get(cell.id);
  return q !== undefined && q.items.length > 0;
}

/** Capture the frame that owns this intent at its first edit, and release
 *  it once the bundle is empty again. A later frame never retargets an
 *  already-captured origin; a frame that carries no binding captures an
 *  explicit absence rather than a later scope. */
function captureBinding(cell: DraftCell): void {
  if (hasIntent(cell)) {
    if (cell.binding === undefined && cell.frame !== undefined) cell.binding = cell.frame;
  } else if (cell.binding !== undefined) {
    cell.binding = undefined;
  }
}

function notifyFamily(family: Family): void {
  family.savedSnapshot = family.saved.filter((record) => {
    const cell = slotsById.get(record.slot);
    return cell !== undefined && hasIntent(cell);
  });
  for (const listener of family.listeners) listener();
  for (const listener of savedListeners) listener();
}

function publish(cell: DraftCell): void {
  cell.snapshot = { text: cell.text, refs: cell.refs, version: cell.version, reserved: cell.reserved !== null };
  const family = families.get(cell.family);
  if (family) notifyFamily(family);
}

/** Drop an idle, empty family — never one holding an in-flight handoff,
 *  parked bundles or a queue with rows. */
function pruneFamily(family: Family): void {
  if (family.listeners.size > 0) return;
  family.saved = family.saved.filter((record) => {
    const cell = slotsById.get(record.slot);
    return cell !== undefined && hasIntent(cell);
  });
  if (family.saved.length > 0) return;
  if (family.slots.some(hasIntent)) return;
  families.delete(family.key);
  for (const cell of family.slots) {
    slotsById.delete(cell.id);
    queues.delete(cell.id);
  }
}

function pruneCell(cell: DraftCell): void {
  const family = families.get(cell.family);
  if (family) pruneFamily(family);
}

function dropSlot(family: Family, cell: DraftCell): void {
  family.slots = family.slots.filter((slot) => slot !== cell);
  slotsById.delete(cell.id);
  queues.delete(cell.id);
}

function park(family: Family, cell: DraftCell, subject?: string): void {
  if (family.saved.some((record) => record.slot === cell.id)) return;
  family.saved = [...family.saved, { slot: cell.id, subject, at: Date.now() }];
}

function removeSaved(family: Family, slot: number): void {
  family.saved = family.saved.filter((record) => record.slot !== slot);
}

/** The keyed draft snapshot — the Composer's `useSyncExternalStore`. */
export function composerDraftState(key: string): ComposerDraft {
  const family = families.get(key);
  return family ? activeOf(family).snapshot : EMPTY_DRAFT;
}

/** The slot a read addresses: the family's visible slot, or one parked
 *  bundle named by `savedIntentsFor(key)`'s `slot`. */
function slotOf(key: string, slot?: number): DraftCell | undefined {
  if (slot !== undefined) return slotsById.get(slot);
  const family = families.get(key);
  return family ? activeOf(family) : undefined;
}

/** Subscribe to one family's active slot, like the queue. */
export function subscribeDraft(key: string, listener: () => void): () => void {
  const family = familyFor(key);
  family.listeners.add(listener);
  return () => {
    family.listeners.delete(listener);
    pruneFamily(family);
  };
}

/** Replace the draft text (an explicit edit). */
export function setComposerDraft(key: string, next: string): void {
  const cell = activeOf(familyFor(key));
  if (cell.text === next) return;
  cell.text = next;
  cell.version += 1;
  captureBinding(cell);
  publish(cell);
  pruneCell(cell);
}

/** Replace the citation refs (an explicit edit). */
export function setComposerRefs(key: string, refs: ThreadRef[]): void {
  const cell = activeOf(familyFor(key));
  if (sameRefs(cell.refs, refs)) return;
  cell.refs = refs;
  cell.version += 1;
  captureBinding(cell);
  publish(cell);
  pruneCell(cell);
}

/** Replace text and refs together (a seed) — one version, one render. */
export function setComposerContent(key: string, text: string, refs: ThreadRef[]): void {
  const cell = activeOf(familyFor(key));
  if (cell.text === text && sameRefs(cell.refs, refs)) return;
  cell.text = text;
  cell.refs = refs;
  cell.version += 1;
  captureBinding(cell);
  publish(cell);
  pruneCell(cell);
}

function sameRefs(a: readonly ThreadRef[], b: readonly ThreadRef[]): boolean {
  return a.length === b.length && a.every((r, i) => r.kind === b[i].kind && r.id === b[i].id);
}

/** An opaque handle to one in-flight handoff reservation. Settlement goes
 *  through the handle, not the scope key, so it still finds its cell when
 *  the scope or frame moves. */
export type DraftReservation = number;

const reservations = new Map<DraftReservation, { cell: DraftCell; version: number }>();
let nextReservation = 0;

/** Reserve the active slot's current draft for one handoff. Returns an
 *  opaque reservation, or null when a handoff is already in flight — a
 *  second submit must not mint a second message for the same original
 *  envelope. */
export function reserveDraft(key: string): DraftReservation | null {
  const cell = activeOf(familyFor(key));
  if (cell.reserved !== null) return null;
  cell.reserved = cell.version;
  publish(cell);
  const token = ++nextReservation;
  reservations.set(token, { cell, version: cell.version });
  return token;
}

/** Settle a reserved handoff. `accepted` clears the text/refs only when
 *  the version is still the one reserved, so a later explicit edit is
 *  never discarded; a rejected handoff keeps the envelope for an explicit
 *  retry. Either way only this reservation is released, and the cell is
 *  found through the handle even after a scope or frame move. */
export function settleDraft(reservation: DraftReservation, accepted: boolean): void {
  const held = reservations.get(reservation);
  if (!held) return;
  reservations.delete(reservation);
  const cell = held.cell;
  if (cell.reserved !== held.version) return;
  cell.reserved = null;
  if (accepted && cell.version === held.version) {
    cell.text = "";
    cell.refs = EMPTY_REFS;
    cell.version += 1;
  }
  captureBinding(cell);
  publish(cell);
}

function queueOf(id: number): Queue {
  let q = queues.get(id);
  if (!q) {
    q = { items: EMPTY };
    queues.set(id, q);
  }
  return q;
}

/** The addressed slot's current rows (stable identity until a change).
 *  With no `slot`, the family's visible slot. */
export function attachItems(key: string, slot?: number): AttachItem[] {
  const cell = slotOf(key, slot);
  if (!cell) return EMPTY;
  return queues.get(cell.id)?.items ?? EMPTY;
}

/** Subscribe to one family's active queue — the Composer's
 *  `useSyncExternalStore`. */
export function subscribeAttach(key: string, listener: () => void): () => void {
  const family = familyFor(key);
  family.listeners.add(listener);
  return () => {
    family.listeners.delete(listener);
    pruneFamily(family);
  };
}

/** The unresolved rows (uploading/failed) — the send gate. */
export function attachHold(key: string): AttachItem[] {
  return attachItems(key).filter((i) => i.status !== "ready");
}

/** The ready rows one submit is about to carry, frozen by row identity:
 *  the `{id}` array for the send plus each row's key and upload
 *  generation. File ids are not row identity — two distinct rows can
 *  legitimately hold the same dedup id. */
export interface ReadyCapture {
  ids: { id: string }[];
  rows: { key: string; generation: number }[];
}

export function captureReady(key: string, expectedDestination?: AttachDestination): ReadyCapture {
  const ready = attachItems(key).filter((i) => i.status === "ready" && i.id !== undefined);
  if (ready.some((item) => !sameAttachDestination(item.destination, expectedDestination))) {
    throw new Error("A ready attachment belongs to another conversation or scope — restore its original draft before sending.");
  }
  const rows = ready;
  return {
    ids: rows.map((i) => ({ id: i.id! })),
    rows: rows.map((i) => ({ key: i.key, generation: i.generation })),
  };
}

/** Remove exactly the rows a completed send carried: the captured
 *  key+generation pairs, which are minted module-wide and so name one row
 *  even after a scope or frame move. A later row that happens to share
 *  the retained file id is untouched, as are failed/uploading rows. */
export function settleReady(capture: ReadyCapture): void {
  if (capture.rows.length === 0) return;
  const carried = new Set(capture.rows.map((r) => `${r.key}:${r.generation}`));
  for (const [id, q] of queues) {
    const next = q.items.filter((a) => !(a.status === "ready" && carried.has(`${a.key}:${a.generation}`)));
    if (next.length === q.items.length) continue;
    q.items = next;
    const cell = slotsById.get(id);
    if (cell) {
      captureBinding(cell);
      publish(cell);
    }
  }
}

/** One upload's completion: only the row that still carries this exact
 *  key and generation and is still uploading may be patched. Keys are
 *  minted module-wide, so the row is found wherever its frame now lives —
 *  a disposed-and-recreated queue cannot re-mint one. */
function patch(
  itemKey: string,
  generation: number,
  next: Partial<AttachItem> & Pick<AttachItem, "status">,
): void {
  for (const [id, q] of queues) {
    let changed = false;
    const items = q.items.map((a) => {
      if (a.key !== itemKey || a.status !== "uploading" || a.generation !== generation) return a;
      changed = true;
      return { ...a, ...next };
    });
    if (!changed) continue;
    q.items = items;
    const cell = slotsById.get(id);
    if (cell) publish(cell);
    return;
  }
}

function moveComposerSlot(from: string, to: string, slotId: number, subject?: string): boolean {
  if (from === to) return true;
  const source = families.get(from);
  const slot = slotsById.get(slotId);
  if (!source || !slot || slot.family !== from || !source.slots.includes(slot)) return false;
  const dest = familyFor(to);
  if (dest.slots.some(hasIntent)) {
    park(dest, slot, subject);
    notifyFamily(dest);
    return false;
  }
  const destActive = activeOf(dest);
  const destFrame = frameKey(destActive.frame);
  const activate = destFrame === "?" || frameKey(slot.frame) === destFrame;
  const movedRecord = source.saved.find((record) => record.slot === slot.id);
  source.saved = source.saved.filter((record) => record.slot !== slot.id);
  source.slots = source.slots.filter((candidate) => candidate !== slot);
  slot.family = to;
  dest.slots.push(slot);
  if (movedRecord && !dest.saved.some((record) => record.slot === slot.id)) dest.saved.push(movedRecord);
  if (activate) {
    removeSaved(dest, destActive.id);
    dropSlot(dest, destActive);
    dest.activeId = slot.id;
    removeSaved(dest, slot.id);
  } else {
    park(dest, slot, subject);
  }
  if (source.activeId === slot.id) {
    const fresh = newCell(source, undefined);
    source.activeId = fresh.id;
  }
  notifyFamily(source);
  notifyFamily(dest);
  return activate;
}

function moveComposerRowOwner(itemKey: string, generation: number, to: string, subject?: string): void {
  for (const [id, queue] of queues) {
    const owned = queue.items.some(
      (item) => item.key === itemKey && item.status === "uploading" && item.generation === generation,
    );
    if (!owned) continue;
    const cell = slotsById.get(id);
    if (cell) moveComposerSlot(cell.family, to, cell.id, subject);
    return;
  }
}

function uploadStillOwned(itemKey: string, generation: number, controller: AbortController): boolean {
  if (controller.signal.aborted) return false;
  for (const queue of queues.values()) {
    if (queue.items.some((item) => item.key === itemKey && item.status === "uploading" && item.generation === generation)) {
      return true;
    }
  }
  return false;
}

function uploadDestination(destination: AttachDestination | undefined): ChatFileDestination | undefined {
  if (destination === undefined) return undefined;
  if (destination.app === undefined) {
    if (destination.conversation !== undefined || destination.subject !== undefined || destination.prepareConversation) {
      throw new Error("An app attachment cannot fall back to the Home upload destination.");
    }
    return undefined;
  }
  if (destination.conversation === undefined) {
    throw new Error("This app attachment has no conversation destination.");
  }
  return {
    install_id: destination.app.install_id,
    conversation: destination.conversation,
    ...(destination.app.context_id !== undefined ? { context_id: destination.app.context_id } : {}),
  };
}

function startUpload(
  itemKey: string,
  file: File,
  generation: number,
  capturedDestination?: AttachDestination,
): void {
  const controller = new AbortController();
  controllers.set(itemKey, controller);
  const stillOwned = () => uploadStillOwned(itemKey, generation, controller);
  void (async () => {
    try {
      let destination = capturedDestination;
      if (destination?.app && destination.conversation === undefined) {
        if (!destination.prepareConversation) throw new Error("This app attachment has no prepared conversation destination.");
        const conversation = await destination.prepareConversation(stillOwned);
        if (!stillOwned()) return;
        destination = { ...destination, conversation, prepareConversation: undefined };
        if (destination.app && destination.subject !== undefined) {
          moveComposerRowOwner(
            itemKey,
            generation,
            `app|${destination.app.install_id}|${conversation}`,
            destination.subject,
          );
        }
        patch(itemKey, generation, { status: "uploading", destination });
      }
      if (destination?.app && destination.conversation === undefined) {
        throw new Error("This app attachment has no conversation destination.");
      }
      if (!stillOwned()) return;
      const row: ChatFileRow = await api.chatFileUpload(
        file,
        controller.signal,
        uploadDestination(destination),
      );
      if (!stillOwned()) return;
      controllers.delete(itemKey);
      patch(itemKey, generation, { status: "ready", id: row.id, name: row.name, mime: row.mime, destination });
      try {
        destination?.refreshCapability?.();
      } catch {
        // Capability refresh is advisory; the daemon has already returned
        // this retained row and will re-prove authorization on later use.
      }
    } catch (e: unknown) {
      controllers.delete(itemKey);
      // A cancel/remove ends this row's life even when it was waiting for
      // conversation preparation: its late result must not select, upload,
      // resurrect or fail a row that is no longer its own.
      if (!stillOwned()) return;
      if (e instanceof ApiError && e.status >= 400 && e.status < 600) {
        try {
          capturedDestination?.refreshCapability?.();
        } catch {
          // Capability refresh is advisory; the upload error remains visible.
        }
      }
      patch(itemKey, generation, {
        status: "failed",
        error: e instanceof Error ? e.message : String(e),
      });
    }
  })();
}

/** Queue files picked or dropped: each lands as its own row and uploads
 *  immediately; a pre-flight refusal lands as a failed row (never
 *  silently dropped). */
export function addFiles(key: string, files: Iterable<File>, destination?: AttachDestination): void {
  const cell = activeOf(familyFor(key));
  const q = queueOf(cell.id);
  let have = q.items.length;
  const added: AttachItem[] = [];
  for (const file of files) {
    const itemKey = `a${++nextItemKey}`;
    const problem = attachRefusal(file.name, file.size) ?? attachFits(have, 1);
    have += 1;
    if (problem !== null) {
      added.push({
        key: itemKey,
        name: file.name,
        size: file.size,
        status: "failed",
        file,
        error: problem,
        generation: 0,
        destination: cloneDestination(destination),
      });
      continue;
    }
    const generation = ++nextGeneration;
    added.push({
      key: itemKey,
      name: file.name,
      size: file.size,
      status: "uploading",
      file,
      generation,
      destination: cloneDestination(destination),
    });
  }
  if (added.length === 0) return;
  q.items = [...q.items, ...added];
  captureBinding(cell);
  publish(cell);
  for (const a of added) {
    if (a.status === "uploading") startUpload(a.key, a.file, a.generation, a.destination);
  }
}

function cloneDestination(destination: AttachDestination | undefined): AttachDestination | undefined {
  if (!destination) return undefined;
  return {
    ...destination,
    app: destination.app ? { ...destination.app } : undefined,
  };
}

/** Retry one failed row under a fresh generation — the old attempt's
 *  completion can no longer patch it. */
export function retryAttach(key: string, itemKey: string): void {
  const family = families.get(key);
  if (!family) return;
  const cell = activeOf(family);
  const q = queues.get(cell.id);
  if (!q) return;
  const item = q.items.find((a) => a.key === itemKey);
  if (!item || item.status !== "failed") return;
  const generation = ++nextGeneration;
  q.items = q.items.map((a) =>
    a.key === itemKey ? { ...a, status: "uploading" as const, error: undefined, generation } : a,
  );
  publish(cell);
  startUpload(itemKey, item.file, generation, item.destination);
}

/** Remove one row. Its upload, if any, is now orphaned: a late
 *  completion finds no row and cannot resurrect it. */
export function removeAttach(key: string, itemKey: string): void {
  const family = families.get(key);
  if (!family) return;
  const cell = activeOf(family);
  const q = queues.get(cell.id);
  if (!q) return;
  const next = q.items.filter((a) => a.key !== itemKey);
  if (next.length === q.items.length) return;
  // Cancel only this row's transport. The removal is a local detach,
  // never a claim that the retained file was deleted on the host.
  controllers.get(itemKey)?.abort();
  controllers.delete(itemKey);
  q.items = next;
  captureBinding(cell);
  publish(cell);
  pruneFamily(family);
}

/** A newer unsent intent kept whole because its destination already owned
 *  intent. The bundle (text, refs and queue) stays under `slot` — never
 *  split — and this record makes it explicitly reachable from the family
 *  that parked it, not stored behind a key the picker no longer shows. */
export interface SavedIntent {
  /** Stable slot id holding the parked bundle; pass it to
   *  `restoreSavedIntent` to bring that exact bundle into the visible
   *  slot. */
  slot: number;
  /** The unsaved subject the bundle was created under, when known. */
  subject?: string;
  at: number;
}

/** Subscribe a pane to the saved-intent records (module state). */
export function subscribeSavedIntents(listener: () => void): () => void {
  savedListeners.add(listener);
  return () => {
    savedListeners.delete(listener);
  };
}

/** The saved bundles reachable from `key`, in the order they were parked.
 *  A stable snapshot until the store changes — `useSyncExternalStore`
 *  reads it directly. */
export function savedIntentsFor(key: string): SavedIntent[] {
  const family = families.get(key);
  return family ? family.savedSnapshot : EMPTY_SAVED;
}

/** The first saved bundle reachable from `key`, or null when there is
 *  none. */
export function savedIntentFor(key: string): SavedIntent | null {
  return savedIntentsFor(key)[0] ?? null;
}

/** Move every unsent bundle of `from` to `to` — an unsaved subject
 *  becoming the conversation its first send created. Text, refs and the
 *  queue are ONE unit per slot: either the whole bundle moves, or nothing
 *  moves and the bundle is recorded as explicitly reachable from the
 *  destination (never newer source files beside a different destination
 *  draft). In-flight uploads keep their row identity and transport (keys,
 *  generations and controllers are module-wide), so a completion still
 *  lands on its own row. */
export function moveComposerScope(from: string, to: string, subject?: string): boolean {
  if (from === to) return true;
  const source = families.get(from);
  if (!source) return true;
  const movable = source.slots.filter(hasIntent);
  if (movable.length === 0) return true;
  const dest = familyFor(to);
  if (dest.slots.some(hasIntent)) {
    // The destination owns intent: keep every source bundle whole where
    // it is and make each one reachable from the destination instead of
    // mixing the two intents.
    for (const slot of movable) park(dest, slot, subject);
    notifyFamily(dest);
    return false;
  }
  const destActive = activeOf(dest);
  const destFrame = frameKey(destActive.frame);
  const sourceActiveId = source.activeId;
  for (const slot of movable) {
    source.slots = source.slots.filter((s) => s !== slot);
    slot.family = to;
    dest.slots.push(slot);
  }
  // Saved records that named a moved slot follow it into the destination;
  // every parked intent remains explicitly reachable after the family move.
  const movedRecords = source.saved.filter((record) => movable.some((slot) => slot.id === record.slot));
  source.saved = source.saved.filter((record) => !movable.some((slot) => slot.id === record.slot));
  for (const record of movedRecords) {
    if (!dest.saved.some((saved) => saved.slot === record.slot)) dest.saved.push(record);
  }
  const preferred =
    movable.find((s) => frameKey(s.frame) === destFrame && destFrame !== "?") ??
    movable.find((s) => s.id === sourceActiveId) ??
    movable[0];
  if (destActive.id !== preferred.id) dropSlot(dest, destActive);
  dest.activeId = preferred.id;
  removeSaved(dest, preferred.id);
  if (!source.slots.some((s) => s.id === source.activeId)) {
    const fresh = newCell(source, undefined);
    source.activeId = fresh.id;
  }
  notifyFamily(dest);
  notifyFamily(source);
  return true;
}

/** Swap two slots' bundles in place, cell identities preserved so every
 *  subscription and reservation still points at the right state. The
 *  slots' own frames stay put — only the bundles move. */
function swapBundles(a: DraftCell, b: DraftCell): void {
  const text = a.text;
  const refs = a.refs;
  const version = a.version;
  const reserved = a.reserved;
  const binding = a.binding;
  a.text = b.text;
  a.refs = b.refs;
  a.version = b.version;
  a.reserved = b.reserved;
  a.binding = b.binding;
  b.text = text;
  b.refs = refs;
  b.version = version;
  b.reserved = reserved;
  b.binding = binding;
  for (const held of reservations.values()) {
    if (held.cell === a) held.cell = b;
    else if (held.cell === b) held.cell = a;
  }
  const qa = queues.get(a.id)?.items ?? EMPTY;
  const qb = queues.get(b.id)?.items ?? EMPTY;
  if (qa !== qb) {
    queueOf(a.id).items = qb;
    queueOf(b.id).items = qa;
  }
  publish(a);
  publish(b);
}

/** Swap a saved bundle into the family's visible slot. The bundle that
 *  was visible is kept reachable through the same record, so an explicit
 *  restore loses neither intent; a swap that leaves nothing behind clears
 *  the record. `slot` names one saved bundle; omitted, the first is
 *  restored. */
export function restoreSavedIntent(to: string, slot?: number): boolean {
  const family = familyFor(to);
  const record = slot === undefined ? family.saved[0] : family.saved.find((s) => s.slot === slot);
  if (!record) return false;
  const saved = slotsById.get(record.slot);
  if (!saved || !hasIntent(saved)) {
    removeSaved(family, record.slot);
    notifyFamily(family);
    return false;
  }
  const active = activeOf(family);
  if (saved === active) {
    removeSaved(family, saved.id);
    notifyFamily(family);
    return false;
  }
  swapBundles(active, saved);
  if (!hasIntent(saved)) removeSaved(family, saved.id);
  notifyFamily(family);
  return true;
}

/** Register the pane's current frame for this family. A displayed
 *  installation/context/absence frame owns its own live slot: entering a
 *  different registered frame parks the old bundle (whole, under its
 *  original owner) and exposes a fresh slot for the new frame. A
 *  transient `undefined` frame (still loading or failed) is not a
 *  registered absence and never moves the active slot. */
export function setComposerScope(key: string, scope: ComposerScope | null | undefined): void {
  const family = familyFor(key);
  const active = activeOf(family);
  const next = frameKey(scope);
  if (frameKey(active.frame) === next) return;
  if (scope === undefined) return;
  if (active.frame === undefined) {
    // The intent was edited before any frame was displayed: the first
    // genuinely registered frame becomes its origin.
    active.frame = scope;
    captureBinding(active);
    publish(active);
    return;
  }
  const parked = family.slots.find((slot) => slot !== active && frameKey(slot.frame) === next);
  if (parked) {
    if (hasIntent(active)) park(family, active);
    else dropSlot(family, active);
    family.activeId = parked.id;
    removeSaved(family, parked.id);
    notifyFamily(family);
    return;
  }
  if (hasIntent(active)) {
    park(family, active);
    const fresh = newCell(family, scope);
    family.activeId = fresh.id;
    notifyFamily(family);
    return;
  }
  active.frame = scope;
  active.binding = undefined;
  notifyFamily(family);
}

/** The binding captured for this family's intent: `null` for an explicit
 *  captured absence, `undefined` when no origin was captured. A send
 *  constructed from a captured intent uses this owner, never the frame
 *  that happens to be current. With `slot`, reads one parked bundle named
 *  by `savedIntentsFor(key)`; with none, the visible slot. */
export function composerScope(key: string, slot?: number): ComposerScope | null | undefined {
  return slotOf(key, slot)?.binding;
}

/** The frame the family's visible slot is registered under: `undefined`
 *  while unknown, `null` for a registered absence. */
export function composerFrame(key: string): ComposerScope | null | undefined {
  const family = families.get(key);
  return family ? activeOf(family).frame : undefined;
}

/** The typed draft, keyed like the queue: Home ⇄ Apps ⇄ conversation
 *  switches restore their own text; another scope never receives it.
 *  With `slot`, reads one parked bundle named by `savedIntentsFor(key)`. */
export function composerDraft(key: string, slot?: number): string {
  return slotOf(key, slot)?.text ?? "";
}

/** The draft's citation refs — restored with the same original key, so
 *  a remount never drops a cited row the operator attached. With `slot`,
 *  reads one parked bundle named by `savedIntentsFor(key)`. */
export function composerRefs(key: string, slot?: number): ThreadRef[] {
  return slotOf(key, slot)?.refs ?? EMPTY_REFS;
}
