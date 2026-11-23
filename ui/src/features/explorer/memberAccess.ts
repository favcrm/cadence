import { ApiError } from "../../lib/apiError";

/**
 * CAD-2215: the operator's per-conversation member access for one installed
 * app. A grant is exactly one workspace member subject and one exact app
 * conversation id; there is no app-wide or context-wide grant.
 *
 * Everything a screen renders comes from here, and every field that decides
 * a write is checked: an older, truncated or reshaped answer is
 * `MemberAccessUnreadable`, never an empty list, a silent success or an
 * invented member. The daemon stays the authority for the live roster, the
 * approved digest and the write itself; this module only refuses to guess
 * about them.
 */

export type GrantState = "active" | "revoked";

export interface MemberAccessConversation {
  id: string;
  context_id: string | null;
  /** The installation's canonical General conversation (no app context). */
  general: boolean;
  title: string | null;
}

export interface MemberAccessGrant {
  member_sub: string;
  install_id: string;
  conversation_id: string;
  context_id: string | null;
  install_digest: string;
  state: GrantState;
  /** Epoch seconds (the daemon's native f64). */
  granted_at: number;
  granted_by: string;
  revoked_at: number | null;
  revoked_by: string | null;
  /** The daemon's current verdict for this row (approved digest, live conversation…). */
  usable: boolean;
  /** Machine token for why the row is not usable; empty when usable. */
  reason: string;
}

/** The only roles the daemon's grant guard proves: anything else is not a
 * member it would grant to, so it is refused rather than displayed. */
export type RosterRole = "owner" | "member";

/** One entry of the daemon-verified current live roster. */
export interface RosterMember {
  subject_id: string;
  name: string;
  role: RosterRole;
}

export interface MemberAccess {
  install_id: string;
  /** The installation's current bundle digest, or null when it cannot be read. */
  digest: string | null;
  /** "" when the installation is present and its digest readable. */
  install_reason: string;
  conversations: MemberAccessConversation[];
  grants: MemberAccessGrant[];
  /**
   * The same verified live roster the daemon's grant guard reads, or null
   * when this operator's session cannot prove it. Null never means "empty
   * roster": it means a grant cannot be created from this page.
   */
  members: RosterMember[] | null;
  /** "" only when the current install/digest is approved and a grant may proceed. */
  grant_reason: string;
}

/** A malformed or unreadable answer. The message is safe to show. */
export class MemberAccessUnreadable extends Error {
  constructor(message = "The board's answer could not be read. Nothing was changed.") {
    super(message);
    this.name = "MemberAccessUnreadable";
  }
}

const DIGEST = /^sha256:[0-9a-f]{64}$/;
const nonEmpty = (value: unknown): value is string => typeof value === "string" && value !== "";

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function conversation(value: unknown): MemberAccessConversation | null {
  const row = record(value);
  if (!row) return null;
  const id = row.id;
  const rawContext = row.context_id;
  const general = row.general;
  const rawTitle = row.title;
  if (!nonEmpty(id) || typeof general !== "boolean") return null;
  const contextId = rawContext === null ? null : nonEmpty(rawContext) ? rawContext : undefined;
  if (contextId === undefined) return null;
  const title = rawTitle === null ? null : typeof rawTitle === "string" ? rawTitle : undefined;
  if (title === undefined) return null;
  return { id, context_id: contextId, general, title };
}

function grant(value: unknown, installId: string): MemberAccessGrant | null {
  const row = record(value);
  if (!row) return null;
  const memberSub = row.member_sub;
  const conversationId = row.conversation_id;
  const rowInstallId = row.install_id;
  const rawContext = row.context_id;
  const installDigest = row.install_digest;
  const rawState = row.state;
  const grantedAt = row.granted_at;
  const grantedBy = row.granted_by;
  const rawRevokedAt = row.revoked_at;
  const rawRevokedBy = row.revoked_by;
  const usable = row.usable;
  const reason = row.reason;
  if (!nonEmpty(memberSub) || !nonEmpty(conversationId)) return null;
  // A row about another installation is not this page's authority.
  if (typeof rowInstallId !== "string" || rowInstallId !== installId) return null;
  const contextId = rawContext === null ? null : nonEmpty(rawContext) ? rawContext : undefined;
  if (contextId === undefined) return null;
  if (!nonEmpty(installDigest)) return null;
  const state = rawState === "active" || rawState === "revoked" ? rawState : null;
  if (state === null) return null;
  if (typeof grantedAt !== "number" || !Number.isFinite(grantedAt) || grantedAt <= 0) return null;
  if (!nonEmpty(grantedBy)) return null;
  const revokedAt =
    rawRevokedAt === null
      ? null
      : typeof rawRevokedAt === "number" && Number.isFinite(rawRevokedAt)
        ? rawRevokedAt
        : undefined;
  if (revokedAt === undefined) return null;
  const revokedBy = rawRevokedBy === null ? null : nonEmpty(rawRevokedBy) ? rawRevokedBy : undefined;
  if (revokedBy === undefined) return null;
  if (typeof usable !== "boolean" || typeof reason !== "string") return null;
  return {
    member_sub: memberSub,
    install_id: rowInstallId,
    conversation_id: conversationId,
    context_id: contextId,
    install_digest: installDigest,
    state,
    granted_at: grantedAt,
    granted_by: grantedBy,
    revoked_at: revokedAt,
    revoked_by: revokedBy,
    usable,
    reason,
  };
}

function parseRosterMember(value: unknown): RosterMember | null {
  const row = record(value);
  if (!row) return null;
  const subjectId = row.subject_id;
  const name = row.name;
  const role = row.role;
  // Only the proven roles parse. An unexpected role is not a member the
  // daemon's guard would grant to, and presenting it would relabel it, so
  // the row is refused and `parseMemberAccess` treats the whole answer as
  // unreadable rather than showing an incomplete roster as complete.
  const provenRole = role === "owner" || role === "member" ? role : null;
  if (!nonEmpty(subjectId) || typeof name !== "string" || provenRole === null) return null;
  return { subject_id: subjectId, name, role: provenRole };
}

/**
 * Validate the whole `GET …/member-access` body. Throws rather than
 * returning a partial view: a conversation or grant that cannot be read is
 * not the same as one that does not exist.
 */
export function parseMemberAccess(raw: unknown): MemberAccess {
  const body = record(raw);
  if (!body) throw new MemberAccessUnreadable();
  const installId = body.install_id;
  const installReason = body.install_reason;
  const grantReason = body.grant_reason;
  const rawDigest = body.digest;
  const rawMembers = body.members;
  const rawConversations = body.conversations;
  const rawGrants = body.grants;
  if (!nonEmpty(installId)) throw new MemberAccessUnreadable();
  if (!Array.isArray(rawConversations) || !Array.isArray(rawGrants)) throw new MemberAccessUnreadable();
  if (typeof installReason !== "string" || typeof grantReason !== "string") throw new MemberAccessUnreadable();
  const digest = rawDigest === null ? null : typeof rawDigest === "string" && DIGEST.test(rawDigest) ? rawDigest : undefined;
  if (digest === undefined) throw new MemberAccessUnreadable();
  if (rawMembers !== null && !Array.isArray(rawMembers)) throw new MemberAccessUnreadable();

  const conversations: MemberAccessConversation[] = [];
  const seenConversations = new Set<string>();
  for (const value of rawConversations) {
    const row = conversation(value);
    if (!row || seenConversations.has(row.id)) throw new MemberAccessUnreadable();
    seenConversations.add(row.id);
    conversations.push(row);
  }

  const grants: MemberAccessGrant[] = [];
  for (const value of rawGrants) {
    const row = grant(value, installId);
    if (!row) throw new MemberAccessUnreadable();
    grants.push(row);
  }

  let members: RosterMember[] | null = null;
  if (Array.isArray(rawMembers)) {
    members = [];
    const seenSubjects = new Set<string>();
    for (const value of rawMembers) {
      const row = parseRosterMember(value);
      // Every roster row must carry a proven role: an unknown role makes the
      // whole answer unreadable rather than quietly presenting an incomplete
      // roster as complete.
      if (row === null) throw new MemberAccessUnreadable();
      if (seenSubjects.has(row.subject_id)) throw new MemberAccessUnreadable();
      seenSubjects.add(row.subject_id);
      members.push(row);
    }
  }

  return {
    install_id: installId,
    digest,
    install_reason: installReason,
    conversations,
    grants,
    members,
    grant_reason: grantReason,
  };
}

/** The `{install_id, grant}` receipt of a grant write, checked field by field. */
export function parseGrantReceipt(raw: unknown): MemberAccessGrant {
  const body = record(raw);
  if (!body) throw new MemberAccessUnreadable();
  const installId = body.install_id;
  if (!nonEmpty(installId)) throw new MemberAccessUnreadable();
  const row = grant(body.grant, installId);
  if (!row) throw new MemberAccessUnreadable();
  return row;
}

export interface RevokeReceipt {
  install_id: string;
  member_sub: string;
  revoked: boolean;
  grants: MemberAccessGrant[];
}

export function parseRevokeReceipt(raw: unknown): RevokeReceipt {
  const body = record(raw);
  if (!body) throw new MemberAccessUnreadable();
  const installId = body.install_id;
  const memberSub = body.member_sub;
  const revoked = body.revoked;
  const rawGrants = body.grants;
  if (!nonEmpty(installId) || !nonEmpty(memberSub)) throw new MemberAccessUnreadable();
  if (typeof revoked !== "boolean" || !Array.isArray(rawGrants)) throw new MemberAccessUnreadable();
  const grants: MemberAccessGrant[] = [];
  for (const value of rawGrants) {
    const row = grant(value, installId);
    if (!row) throw new MemberAccessUnreadable();
    grants.push(row);
  }
  return { install_id: installId, member_sub: memberSub, revoked, grants };
}

/* ---------------------------- write receipts ----------------------------- */

/**
 * A grant receipt is only evidence for THIS request: the same installation,
 * member, conversation and pinned digest, and an active row. A receipt that
 * names something else is not success, even when a re-read later shows a
 * plausible row (a concurrent actor may have written it).
 */
export function grantReceiptMatches(
  receipt: MemberAccessGrant,
  expected: { installId: string; conversationId: string; memberSub: string; digest: string },
): boolean {
  return (
    receipt.install_id === expected.installId &&
    receipt.member_sub === expected.memberSub &&
    receipt.conversation_id === expected.conversationId &&
    receipt.install_digest === expected.digest &&
    receipt.state === "active"
  );
}

/**
 * A revoke receipt is only evidence for THIS request: the same installation
 * and member, `revoked: true`, and the selected conversation's tombstone in
 * the returned rows. A revoke for another conversation is not this write.
 */
export function revokeReceiptMatches(
  receipt: RevokeReceipt,
  expected: { installId: string; conversationId: string; memberSub: string },
): boolean {
  if (receipt.install_id !== expected.installId || receipt.member_sub !== expected.memberSub) return false;
  if (!receipt.revoked) return false;
  return receipt.grants.some(
    (row) =>
      row.install_id === expected.installId &&
      row.member_sub === expected.memberSub &&
      row.conversation_id === expected.conversationId &&
      row.state === "revoked",
  );
}

/* ------------------------------ derived reads ----------------------------- */

export function activeGrants(access: MemberAccess, conversationId: string): MemberAccessGrant[] {
  return access.grants.filter((row) => row.state === "active" && row.conversation_id === conversationId);
}

export function activeGrant(
  access: MemberAccess,
  conversationId: string,
  memberSub: string,
): MemberAccessGrant | null {
  return (
    access.grants.find(
      (row) => row.state === "active" && row.conversation_id === conversationId && row.member_sub === memberSub,
    ) ?? null
  );
}

export function conversationOf(access: MemberAccess, conversationId: string): MemberAccessConversation | null {
  return access.conversations.find((row) => row.id === conversationId) ?? null;
}

export function rosterMember(access: MemberAccess, subjectId: string): RosterMember | null {
  return access.members?.find((row) => row.subject_id === subjectId) ?? null;
}

/** Display name from the verified roster; null when the roster cannot be
 * read or does not list that subject, so callers label it unavailable. */
export function memberName(access: MemberAccess, subjectId: string): string | null {
  const name = rosterMember(access, subjectId)?.name.trim();
  return name ? name : null;
}

/**
 * Why a grant cannot be created from this state, or null when it may.
 * Every gate is separate so the page can say which one is closed; a missing
 * roster or digest never falls through to "ready".
 */
export function grantBlocker(access: MemberAccess): string | null {
  if (access.install_reason !== "") return access.install_reason;
  if (access.digest === null) return "digest_unavailable";
  if (access.members === null) return "roster_unavailable";
  if (access.members.length === 0) return "roster_empty";
  if (access.grant_reason !== "") return access.grant_reason;
  if (access.conversations.length === 0) return "no_conversations";
  return null;
}

/**
 * The last check between a decision and the write: everything the operator
 * saw is re-proven against the freshly read list. Any doubt refuses the
 * write; the caller shows the sentence and re-renders the fresh list.
 */
export function grantWriteRefusal(
  access: MemberAccess,
  expected: { conversationId: string; memberSub: string; digest: string },
): string | null {
  const blocker = grantBlocker(access);
  if (blocker !== null) return reasonCopy(blocker);
  if (access.digest !== expected.digest) {
    return "The app changed since you opened this. Nothing was granted — review the current access.";
  }
  if (conversationOf(access, expected.conversationId) === null) {
    return "That conversation is no longer available. Nothing was granted.";
  }
  if (access.members === null || rosterMember(access, expected.memberSub) === null) {
    return "That person is not on the workspace's current roster. Nothing was granted.";
  }
  if (activeGrant(access, expected.conversationId, expected.memberSub) !== null) {
    return "That member already has an active grant for this conversation. Nothing was changed.";
  }
  return null;
}

/**
 * Revoke needs only the saved active grant. It must NOT require the
 * conversation to be listed: an archived/detached conversation keeps its
 * grant rows, and those rows are the only way to withdraw that access.
 */
export function revokeWriteRefusal(
  access: MemberAccess,
  expected: { conversationId: string; memberSub: string },
): string | null {
  if (activeGrant(access, expected.conversationId, expected.memberSub) === null) {
    return "That member no longer has an active grant for that conversation. Nothing was changed.";
  }
  return null;
}

/**
 * Active grants whose conversation is no longer listed for the install, so
 * they cannot appear under any conversation selector. They stay revocable by
 * the exact saved pair; the conversation id is evidence, not a live target.
 */
export function orphanActiveGrants(access: MemberAccess): MemberAccessGrant[] {
  const live = new Set(access.conversations.map((row) => row.id));
  return access.grants.filter((row) => row.state === "active" && !live.has(row.conversation_id));
}

const REASON_COPY: Record<string, string> = {
  install_removed: "This app is removed. Restore it before granting access.",
  install_unavailable: "This app's installation record can't be read, so access can't be granted.",
  digest_unavailable: "This installation has no readable package digest, so a grant has nothing to pin.",
  digest_changed: "This app changed since the page loaded. Read the access list again.",
  install_mismatch: "The grant no longer names this installation.",
  not_approved: "The current version isn't approved. Approve it before granting access.",
  capability_revoked: "The app's approval was revoked. Existing grants stay saved but can't be used.",
  capability_unavailable: "The app's approval can't be read right now.",
  roster_unavailable:
    "This session can't prove the current workspace roster, so grants are unavailable. Existing grants can still be revoked.",
  public_session_required:
    "Grants need a verified owner session so the live roster can prove the member. Existing grants can still be revoked.",
  no_conversations: "This app has no conversation to grant yet.",
  roster_empty: "The workspace roster returned no member to grant.",
  not_a_member: "That person is not a current member of this workspace.",
  conversation_unavailable: "That conversation is no longer available for this app.",
  conversation_changed: "That conversation changed. Read the access list again.",
  context_archived: "That conversation's app context was archived.",
  revoked: "This grant was revoked.",
};

/** Human copy for a machine reason token; unknown tokens stay generic. */
export function reasonCopy(reason: string): string {
  return REASON_COPY[reason] ?? "Member access is unavailable for this right now.";
}

/** The known sentence for a server code, or null so the caller keeps its own copy. */
export function reasonCopyFor(code: string | undefined): string | null {
  return code !== undefined && Object.hasOwn(REASON_COPY, code) ? REASON_COPY[code] : null;
}

/**
 * Copy for a refused or failed access write. The daemon's refusals carry
 * command names and internal ids, so only known codes become sentences;
 * everything else stays the plain fallback. A 401/403 is a session refusal,
 * never "nothing exists".
 */
export function memberAccessErrorCopy(cause: unknown, fallback: string): string {
  if (cause instanceof MemberAccessUnreadable) return cause.message;
  if (cause instanceof ApiError) {
    switch (cause.code) {
      case "not_an_owner":
        return "Only a workspace owner can change member access. Nothing was changed.";
      case "not_a_member":
        return "That person is not a current member of this workspace. Nothing was changed.";
      case "tenancy_mismatch":
        return "This session belongs to a different workspace. Nothing was changed.";
      case "capability_unavailable":
        return "A verified owner session is needed to prove the live roster. Nothing was changed.";
      default:
        break;
    }
    if (cause.status === 401 || cause.status === 403) {
      return "This session can't change member access. Nothing was changed.";
    }
    if (cause.status === 404) return "That app or conversation is no longer available. Nothing was changed.";
    if (cause.status === 409 || cause.status === 412) {
      return "The app changed while you were deciding. Read the access list and decide again.";
    }
    if (cause.status >= 500) return fallback;
  }
  return fallback;
}

/** Epoch seconds formatted for a person; null when the value is unreadable. */
export function formatGrantedAt(value: number, locale?: string | string[]): string | null {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) return null;
  const date = new Date(value * 1000);
  if (Number.isNaN(date.getTime())) return null;
  try {
    return new Intl.DateTimeFormat(locale, {
      day: "numeric",
      month: "short",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    }).format(date);
  } catch {
    return null;
  }
}

/** Two initials from a display name; "?" when the roster gave us no name. */
export function initials(name: string | null): string {
  if (!name) return "?";
  const letters = name
    .trim()
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => part[0] ?? "")
    .join("");
  return letters ? letters.toUpperCase() : "?";
}
