/**
 * CAD-2286: who the board shows each document action to. These rules only
 * hide controls; the hosted workspace decides every action. They mirror the
 * server's `rules()`: a type's approver is in force only once its row is
 * confirmed, and a pending or missing row is the baseline (the owner role).
 */
import {
  approverInForce,
  approverMissing,
  canApprove,
  canArchive,
  canCreate,
  canEditTypes,
  canRestore,
  canSubmit,
  documentsViewer,
  isOperatorSession,
  typeFor,
  type DocumentsViewer,
} from "../src/features/documents/documentsAccess";
import type { DocumentState, DocumentType } from "../src/features/documents/documentsClient";

function equal(actual: unknown, expected: unknown, why: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected))
    throw new Error(`${why}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

const owner: DocumentsViewer = { viewerSub: "user-owen", isOwner: true, isOperatorSession: false };
const mia: DocumentsViewer = { viewerSub: "user-mia", isOwner: false, isOperatorSession: false };
const sam: DocumentsViewer = { viewerSub: "user-sam", isOwner: false, isOperatorSession: false };
const operator: DocumentsViewer = { viewerSub: "operator-token:op1", isOwner: true, isOperatorSession: true };
const signedOut: DocumentsViewer = { viewerSub: null, isOwner: false, isOperatorSession: false };

const doc = (stage: DocumentState["stage"], type = "quote"): DocumentState => ({
  type,
  stage,
  submittedVersion: 1,
  approvedVersion: null,
  selfApproved: false,
  deliveredVersion: null,
  deliveredAt: null,
  archivedAt: stage === "archived" ? 1_794_000_000_000 : null,
  rev: 1,
});
const type = (fields: Partial<DocumentType>): DocumentType => ({
  label: "quote",
  requireApproval: true,
  approver: null,
  recipientNote: "",
  archiveAfterDays: 30,
  state: "confirmed",
  rev: 1,
  ...fields,
});

function viewerChecks(): void {
  equal(isOperatorSession("operator-token:abc"), true, "an operator token session");
  equal(isOperatorSession("user-mia"), false, "a member session");
  equal(isOperatorSession(null), false, "no session");
  equal(isOperatorSession(undefined), false, "unknown session");

  equal(
    documentsViewer({ sub: "user-owen", operator: true, readOnly: false }),
    { viewerSub: "user-owen", isOwner: true, isOperatorSession: false },
    "the hosted owner check",
  );
  equal(documentsViewer({ sub: "user-owen", operator: true, readOnly: true }).isOwner, false, "a read-only owner is not an owner here");
  equal(documentsViewer({ sub: "user-mia", operator: false, readOnly: false }).isOwner, false, "a member");
  equal(documentsViewer({ sub: "user-owen", operator: null, readOnly: false }).isOwner, false, "an unknown proof grants nothing");
  equal(
    documentsViewer({ sub: "operator-token:op1", operator: true, readOnly: false }),
    { viewerSub: "operator-token:op1", isOwner: true, isOperatorSession: true },
    "an operator session",
  );
  equal(documentsViewer({ sub: null, operator: null, readOnly: false }).viewerSub, null, "no session");
}

function typeChecks(): void {
  const rows = [type({ label: "quote" }), type({ label: "invoice" })];
  equal(typeFor(rows, doc("in_review", "quote"))?.label, "quote", "matched by the document's type");
  equal(typeFor(rows, doc("in_review", "contract")), undefined, "no row for a new type");
  equal(typeFor([type({ label: "quote" })], doc("in_review", " Quote "))?.label, "quote", "the key is trimmed and lower-cased");

  equal(approverInForce(undefined), null, "no row: the baseline owner role");
  equal(approverInForce(type({ approver: "user-mia", state: "pending" })), null, "a pending row is not in force");
  equal(approverInForce(type({ approver: "user-mia", state: "confirmed" })), "user-mia", "a confirmed row is");
  equal(approverInForce(type({ approver: null, state: "confirmed" })), null, "a confirmed owner-role row");
}

function approveChecks(): void {
  // The plan's case: a member's first type is pending, naming themselves.
  const pendingMia = type({ approver: "user-mia", state: "pending" });
  equal(canApprove(owner, doc("in_review"), pendingMia), true, "pending row: the owner approves (baseline)");
  equal(canApprove(mia, doc("in_review"), pendingMia), false, "pending row: its named member does not");

  const confirmedMia = type({ approver: "user-mia", state: "confirmed" });
  equal(canApprove(mia, doc("in_review"), confirmedMia), true, "confirmed row: the named approver approves");
  equal(canApprove(owner, doc("in_review"), confirmedMia), false, "confirmed row: the owner is not that approver");
  equal(canApprove(sam, doc("in_review"), confirmedMia), false, "confirmed row: another member cannot");

  const confirmedOwner = type({ approver: null, state: "confirmed" });
  equal(canApprove(owner, doc("in_review"), confirmedOwner), true, "owner-role row: the owner approves");
  equal(canApprove(mia, doc("in_review"), confirmedOwner), false, "owner-role row: a member cannot");

  equal(canApprove(owner, doc("in_review"), undefined), true, "no row: the owner approves");
  equal(canApprove(mia, doc("in_review"), undefined), false, "no row: a member cannot");

  equal(canApprove(operator, doc("in_review"), undefined), false, "an operator session never approves, even as owner");
  equal(
    canApprove({ ...operator, viewerSub: "operator-token:op1" }, doc("in_review"), type({ approver: "operator-token:op1" })),
    false,
    "nor when a row names it",
  );
  equal(canApprove(signedOut, doc("in_review"), confirmedMia), false, "no session cannot");

  for (const stage of ["draft", "approved", "delivered", "archived"] as const)
    equal(canApprove(owner, doc(stage), undefined), false, `stage ${stage} is not approvable`);
}

function ownerActionChecks(): void {
  equal(canArchive(owner, doc("delivered")), true, "the owner archives");
  equal(canArchive(owner, doc("draft")), true, "any active stage");
  equal(canArchive(owner, doc("archived")), false, "not an archived one");
  equal(canArchive(mia, doc("delivered")), false, "a member does not");
  equal(canArchive(operator, doc("delivered")), false, "an operator session does not");
  equal(canArchive(documentsViewer({ sub: "user-owen", operator: true, readOnly: true }), doc("delivered")), false, "a read-only owner does not");

  equal(canRestore(owner, doc("archived")), true, "the owner restores");
  equal(canRestore(owner, doc("delivered")), false, "only an archived one");
  equal(canRestore(mia, doc("archived")), false, "a member does not");
  equal(canRestore(operator, doc("archived")), false, "an operator session does not");

  equal(canEditTypes(owner), true, "the owner edits types");
  equal(canEditTypes(mia), false, "a member does not");
  equal(canEditTypes(operator), false, "an operator session does not");
}

function missingChecks(): void {
  const roster = ["user-owen", "user-sam"];
  const gone = type({ approver: "user-mia", state: "confirmed" });
  equal(approverMissing(owner, gone, roster), true, "a confirmed approver who left is flagged to the owner");
  equal(approverMissing(owner, gone, [...roster, "user-mia"]), false, "a current approver is not");
  equal(approverMissing(owner, gone, null), false, "an unknown roster is never read as absent");
  equal(approverMissing(owner, type({ approver: null }), roster), false, "the owner role never leaves");
  equal(approverMissing(owner, type({ approver: "user-mia", state: "pending" }), roster), false, "a pending row is not in force");
  equal(approverMissing(owner, undefined, roster), false, "no row");
  equal(approverMissing(sam, gone, roster), false, "only the owner, who can fix it, is told");
  equal(approverMissing(operator, gone, roster), false, "not an operator session");
  // Approve stays unreachable for that type: nobody current is its approver.
  for (const viewer of [owner, sam]) equal(canApprove(viewer, doc("in_review"), gone), false, "approve is unreachable");
}

function createAndSubmitChecks(): void {
  equal(canCreate(owner), true, "owner can create documents");
  equal(canCreate(mia), true, "member can create documents");
  equal(canCreate(operator), false, "operator session cannot create documents");
  equal(canCreate(signedOut), false, "signed out viewer cannot create documents");

  equal(canSubmit(owner, doc("draft")), true, "owner sees Submit on draft");
  equal(canSubmit(mia, doc("draft")), true, "member sees Submit on draft");
  equal(canSubmit(operator, doc("draft")), false, "operator session does not see Submit");
  equal(canSubmit(signedOut, doc("draft")), false, "signed out viewer does not see Submit");
  for (const stage of ["in_review", "approved", "delivered", "archived"] as const) {
    equal(canSubmit(owner, doc(stage)), false, `stage ${stage} is not submittable`);
    equal(canSubmit(mia, doc(stage)), false, `member cannot submit stage ${stage}`);
  }
}

function main(): void {
  viewerChecks();
  typeChecks();
  approveChecks();
  ownerActionChecks();
  missingChecks();
  createAndSubmitChecks();
  console.log("documentsAccess checks passed");
}

try {
  main();
} catch (err: unknown) {
  setTimeout(() => {
    throw err;
  }, 0);
}
