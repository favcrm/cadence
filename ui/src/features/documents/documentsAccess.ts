import type { DocumentState, DocumentType } from "./documentsClient";

/**
 * CAD-2286: which document controls the board shows this viewer. These rules
 * only hide; the hosted workspace decides every action and refuses a direct
 * request whatever the board rendered.
 *
 * They mirror the server's `rules()` in `document-store.ts`: a type's approver
 * is in force only once its row is `confirmed`. A pending or missing row is the
 * baseline, whose approver is the owner role (`null`).
 *
 * The server also refuses a member who approves their own version
 * (`self_approval`). Neither the file nor the document wire carries the
 * version's author, so that case is left to the server's refusal.
 */

export interface DocumentsViewer {
  /** The board session's subject, or null without one. */
  viewerSub: string | null;
  /** The hosted owner check, as `Members.tsx` reads it. */
  isOwner: boolean;
  /** A platform operator's board session: it reads the board, changes nothing. */
  isOperatorSession: boolean;
}

/** A platform operator's board session is `operator-token:<id>`. */
export function isOperatorSession(sub: string | null | undefined): boolean {
  return typeof sub === "string" && sub.startsWith("operator-token:");
}

/**
 * The viewer from what the board already knows: `meta.session.user.sub` and the
 * operator proof. Owner is `operator === true && !readOnly`; an unknown proof
 * (`null`) grants nothing.
 */
export function documentsViewer(input: {
  sub: string | null | undefined;
  operator: boolean | null;
  readOnly: boolean;
}): DocumentsViewer {
  return {
    viewerSub: input.sub ?? null,
    isOwner: input.operator === true && !input.readOnly,
    isOperatorSession: isOperatorSession(input.sub),
  };
}

/** The server's type key: trimmed and lower-cased. */
const typeKey = (label: string) => label.trim().toLowerCase();

/** The type row a document was submitted under, if one exists. */
export function typeFor(types: readonly DocumentType[], document: DocumentState): DocumentType | undefined {
  const key = typeKey(document.type);
  return types.find((type) => typeKey(type.label) === key);
}

/** The approver in force: a confirmed row's approver, else the owner role (`null`). */
export function approverInForce(type: DocumentType | undefined): string | null {
  return type?.state === "confirmed" ? type.approver : null;
}

/** Create: any signed-in workspace session except a read-only platform operator session. */
export function canCreate(viewer: DocumentsViewer): boolean {
  return viewer.viewerSub !== null && !viewer.isOperatorSession;
}

/** Submit: a draft document, by a non-operator member (creator ownership is enforced by the server). */
export function canSubmit(viewer: DocumentsViewer, document: DocumentState): boolean {
  return document.stage === "draft" && viewer.viewerSub !== null && !viewer.isOperatorSession;
}

/** Approve: a document in review, by its approver in force; never an operator session. */
export function canApprove(viewer: DocumentsViewer, document: DocumentState, type: DocumentType | undefined): boolean {
  if (document.stage !== "in_review" || viewer.isOperatorSession) return false;
  const approver = approverInForce(type);
  return approver === null ? viewer.isOwner : viewer.viewerSub !== null && approver === viewer.viewerSub;
}

/** Archive, restore and type preferences are the workspace owner's; an
 *  operator session admitted as owner changes nothing (`owner_required`). */
const ownerMayChange = (viewer: DocumentsViewer) => viewer.isOwner && !viewer.isOperatorSession;

/** Edit or confirm type preferences. */
export function canEditTypes(viewer: DocumentsViewer): boolean {
  return ownerMayChange(viewer);
}

/** Archive: the owner, for a document that is not archived yet. */
export function canArchive(viewer: DocumentsViewer, document: DocumentState): boolean {
  return ownerMayChange(viewer) && document.stage !== "archived";
}

/** Restore: the owner, for an archived document. */
export function canRestore(viewer: DocumentsViewer, document: DocumentState): boolean {
  return ownerMayChange(viewer) && document.stage === "archived";
}

/**
 * A confirmed type whose approver is no longer a member: nobody can approve it
 * until the owner updates the type, so the owner (who can) is told. `roster`
 * is the current member ids, or null when it could not be read; unknown is
 * never read as absent.
 */
export function approverMissing(
  viewer: DocumentsViewer,
  type: DocumentType | undefined,
  roster: readonly string[] | null,
): boolean {
  if (!canEditTypes(viewer) || roster === null) return false;
  const approver = approverInForce(type);
  return approver !== null && !roster.includes(approver);
}
