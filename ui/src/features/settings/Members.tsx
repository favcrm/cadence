import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { ApiError, errorMessage } from "../../lib/apiError";
import { useLocale, useTranslate } from "../../lib/locale";
import { onPrivateSessionReset, privateFetch, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import Button from "../../ui/Button";
import PageState from "../../ui/PageState";
import Select from "../../ui/Select";
import Toast, { type ToastMsg } from "../../ui/Toast";
import DrawerShell from "../app-shell/shared/DrawerShell";
import type { Viewer } from "../projects/work";
import "./settings-layout.css";
import "./members.css";

/**
 * CAD-2115 Settings → Members on a hosted board.
 *
 * The roster is the board's `/api/team/*` surface: `/members` answers every
 * member, with addresses only to an owner; `/invites` and every write answer an
 * owner alone. So ownership is read from the board's own proof (`viewer`), and
 * each write is still the server's decision. A member sees the roster with no
 * controls; the server refuses a direct write whatever this page renders.
 *
 * The invite email outcome is part of the invite wire shape: the board keeps
 * `emailStatus` on a pending invite that was refused or whose delivery is
 * unconfirmed, so the row is flagged here for every owner, not only the one who
 * sent it.
 */

type Translate = (english: string) => string;
type FormatDate = (value: string, options?: Intl.DateTimeFormatOptions) => string;
type Role = "owner" | "member";

type Member = {
  userId: string;
  name: string;
  /** Only an owner's roster carries addresses. */
  email?: string;
  role: Role;
  joinedAt: string;
};

type Invite = {
  id: string;
  email: string;
  role: Role;
  expiresAt: string;
  /** Set by the board when the last email was refused or unconfirmed. */
  emailStatus?: "failed" | "uncertain";
};

type Drawer = { kind: "member"; id: string } | { kind: "invite"; id: string } | { kind: "newInvite" };

/** One drawer at a time. The token names this opening, so a write that started
 *  under an earlier one cannot close the drawer in use now. */
type OpenDrawer = { drawer: Drawer; token: number };

type ToastKind = ToastMsg["kind"];

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isRole(value: unknown): value is Role {
  return value === "owner" || value === "member";
}

/** The refusal's code, whether the board nested it under `error` or not. */
function codeOf(body: unknown): string | undefined {
  if (!isRecord(body)) return undefined;
  if (typeof body.code === "string") return body.code;
  const error = body.error;
  if (!isRecord(error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

function refused(parsed: unknown, response: Response): ApiError {
  return new ApiError(errorMessage(parsed, `${response.status} ${response.statusText}`), response.status, {
    code: codeOf(parsed),
  });
}

async function boardRead(path: string, signal?: AbortSignal): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(path, { headers: sessionHeaders() }, operation);
    const parsed: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw refused(parsed, response);
    return parsed;
  }, signal);
}

async function boardWrite(path: string, body: object): Promise<unknown> {
  return withPrivateOperation(async (operation) => {
    const response = await privateFetch(
      path,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
          ...sessionHeaders(),
        },
        body: JSON.stringify(body),
      },
      operation,
    );
    const parsed: unknown = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw refused(parsed, response);
    // A write's answer is its receipt: unreadable bytes prove nothing happened to
    // the record, so no caller may report success from them.
    if (parsed === null)
      throw new ApiError("The board answered without a readable receipt.", response.status);
    return parsed;
  });
}

function parseMember(value: unknown): Member {
  if (!isRecord(value)) throw new Error("unexpected member row");
  const role = value.role;
  if (typeof value.userId !== "string" || typeof value.joinedAt !== "string" || !isRole(role))
    throw new Error("unexpected member row");
  return {
    userId: value.userId,
    name: typeof value.name === "string" ? value.name : "",
    email: typeof value.email === "string" ? value.email : undefined,
    role,
    joinedAt: value.joinedAt,
  };
}

function parseMembers(payload: unknown): Member[] {
  if (!isRecord(payload) || !Array.isArray(payload.members)) throw new Error("unexpected roster");
  return payload.members.map(parseMember);
}

function parseInvite(value: unknown): Invite {
  if (!isRecord(value)) throw new Error("unexpected invite row");
  const role = value.role;
  if (
    typeof value.id !== "string" ||
    typeof value.email !== "string" ||
    typeof value.expiresAt !== "string" ||
    !isRole(role)
  )
    throw new Error("unexpected invite row");
  // An older board sends no outcome; anything unrecognised is no known problem.
  const emailStatus =
    value.emailStatus === "failed" || value.emailStatus === "uncertain" ? value.emailStatus : undefined;
  return {
    id: value.id,
    email: value.email,
    role,
    expiresAt: value.expiresAt,
    ...(emailStatus ? { emailStatus } : {}),
  };
}

function parseInvites(payload: unknown): Invite[] {
  if (!isRecord(payload) || !Array.isArray(payload.invites)) throw new Error("unexpected invites");
  return payload.invites.map(parseInvite);
}

function parseInvitePayload(payload: unknown): Invite {
  if (!isRecord(payload)) throw new Error("unexpected invite");
  return parseInvite(payload.invite);
}

function joinedText(iso: string, formatDate: FormatDate): string | null {
  if (Number.isNaN(new Date(iso).getTime())) return null;
  return formatDate(iso, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
}

function expiryText(iso: string, formatDate: FormatDate): string | null {
  if (Number.isNaN(new Date(iso).getTime())) return null;
  return formatDate(iso, { day: "numeric", month: "short", timeZone: "UTC" });
}

function initials(name: string, email?: string): string {
  const source = name.trim() || email?.trim() || "";
  const words = source.split(/\s+/).filter(Boolean);
  if (words.length === 0) return "?";
  return words
    .slice(0, 2)
    .map((word) => Array.from(word)[0] ?? "")
    .join("")
    .toUpperCase();
}

function displayName(member: Pick<Member, "name" | "email">, t: Translate): string {
  return member.name.trim() || member.email || t("Member");
}

function roleName(role: Role, t: Translate): string {
  return role === "owner" ? t("Owner") : t("Member");
}

function roleLine(role: Role, expiresAt: string | null, t: Translate): string {
  const expiry = expiresAt ? t("Expires {date}").replace("{date}", () => expiresAt) : null;
  return [roleName(role, t), expiry].filter((part): part is string => part !== null).join(" · ");
}


/** The refusals every Members write can meet, before its own fallback. */
function sharedFailure(error: unknown, t: Translate): string | null {
  if (!(error instanceof ApiError)) return null;
  if (error.code === "forbidden" || error.code === "owner_required")
    return t("Only a workspace owner can manage members.");
  if (error.code === "membership_changed")
    return t("Your access changed. Reload the page and try again.");
  return null;
}

function roleFailure(error: unknown, t: Translate): string {
  const code = error instanceof ApiError ? error.code : null;
  if (code === "last_owner") return t("The last owner can’t be changed. Make someone else an owner first.");
  if (code === "not_found") return t("That person is no longer a member. Refresh and try again.");
  return sharedFailure(error, t) ?? t("Couldn’t save the role. Try again.");
}

function removeFailure(error: unknown, t: Translate): string {
  const code = error instanceof ApiError ? error.code : null;
  if (code === "last_owner") return t("The last owner can’t be removed.");
  if (code === "not_found") return t("That person is no longer a member. Refresh and try again.");
  return sharedFailure(error, t) ?? t("Couldn’t remove this member. Try again.");
}

function inviteFieldFailure(code: string | undefined, t: Translate): string | null {
  if (code === "member_exists") return t("This address is already a member.");
  if (code === "invite_pending") return t("This address already has a pending invite. Resend or revoke it instead.");
  if (code === "validation_failed") return t("Enter a valid email address.");
  return null;
}

function inviteFailure(error: unknown, t: Translate): string {
  const code = error instanceof ApiError ? error.code : null;
  if (code === "invite_email_failed") return t("The invitation email was refused. Nothing was sent — try again.");
  if (code === "invite_email_uncertain") return t("We couldn’t confirm the invitation email was sent.");
  return sharedFailure(error, t) ?? t("Couldn’t send the invite. Try again.");
}

function revokeFailure(error: unknown, t: Translate): string {
  const code = error instanceof ApiError ? error.code : null;
  if (code === "not_found") return t("This invite is no longer pending. Refresh and try again.");
  return sharedFailure(error, t) ?? t("Couldn’t revoke this invite. Try again.");
}

/** Escape cancels an in-drawer confirmation before the drawer's own Escape closes it. */
function useConfirmEscape(active: boolean, cancel: () => void): void {
  useEffect(() => {
    if (!active) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      cancel();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [active, cancel]);
}

export default function Members({ viewer }: { viewer: Viewer }) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  const canManage = viewer.operator === true && !viewer.readOnly;
  const [members, setMembers] = useState<Member[] | null>(null);
  const [invites, setInvites] = useState<Invite[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<OpenDrawer | null>(null);
  const openSeq = useRef(0);
  const showDrawer = useCallback((drawer: Drawer) => {
    setOpen({ drawer, token: ++openSeq.current });
  }, []);
  const [toast, setToast] = useState<ToastMsg | null>(null);
  const toastTimer = useRef(0);
  const inviteButton = useRef<HTMLButtonElement | null>(null);
  const searchField = useRef<HTMLInputElement | null>(null);
  const focusInvite = useRef(false);
  const focusRecord = useRef<string | null>(null);

  const say = useCallback((kind: ToastKind, text: string) => {
    setToast({ kind, text });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4200);
  }, []);

  useEffect(() => () => window.clearTimeout(toastTimer.current), []);
  useEffect(() => onPrivateSessionReset(() => {
    setMembers(null);
    setInvites(null);
    setOpen(null);
    setToast(null);
    setQuery("");
  }), []);

  const readSeq = useRef(0);
  const refresh = useCallback(
    async (signal?: AbortSignal) => {
      // A post-write read can land after a newer one; only the latest commits.
      const seq = ++readSeq.current;
      setLoading(true);
      setFailed(false);
      try {
        const [roster, pending] = await Promise.all([
          boardRead("/api/team/members", signal).then(parseMembers),
          canManage ? boardRead("/api/team/invites", signal).then(parseInvites) : Promise.resolve<Invite[]>([]),
        ]);
        if (signal?.aborted || seq !== readSeq.current) return;
        setMembers(roster);
        setInvites(pending);
      } catch (error) {
        if (signal?.aborted || (error instanceof DOMException && error.name === "AbortError")) return;
        if (seq !== readSeq.current) return;
        setFailed(true);
      } finally {
        if (!signal?.aborted && seq === readSeq.current) setLoading(false);
      }
    },
    [canManage],
  );

  useEffect(() => {
    if (viewer.operator === null) return;
    const controller = new AbortController();
    void refresh(controller.signal);
    return () => controller.abort();
  }, [viewer.operator, refresh, attempt]);

  // The opening that is live now, readable from a write that started earlier.
  const openRef = useRef<OpenDrawer | null>(null);
  useEffect(() => {
    openRef.current = open;
  }, [open]);

  /**
   * Close the drawer only if it is still the opening that asked. A write can
   * resolve after its drawer was closed or reopened; it must not dismiss the
   * drawer the person is using now.
   */
  const closeDrawer = useCallback((token?: number) => {
    const current = openRef.current;
    if (token !== undefined && token !== current?.token) return;
    const owner = current?.drawer ?? null;
    const invite = focusInvite.current || (owner !== null && owner.kind !== "member");
    const record = focusRecord.current ?? (owner?.kind === "member" ? owner.id : null);
    focusInvite.current = false;
    focusRecord.current = null;
    openRef.current = null;
    setOpen(null);
    window.setTimeout(() => {
      if (record !== null) {
        const row = Array.from(document.querySelectorAll<HTMLElement>("[data-record-id]")).find(
          (element) => element.getAttribute("data-record-id") === record,
        );
        if (row) {
          row.focus();
          return;
        }
      }
      if (invite || record !== null) inviteButton.current?.focus();
    }, 80);
  }, []);

  const visible = useMemo(() => {
    const roster = members ?? [];
    const needle = query.trim().toLowerCase();
    if (!needle) return roster;
    return roster.filter((member) => `${member.name} ${member.email ?? ""}`.toLowerCase().includes(needle));
  }, [members, query]);

  const soleOwner = useCallback(
    (member: Member) => member.role === "owner" && (members ?? []).filter((row) => row.role === "owner").length === 1,
    [members],
  );

  const memberSaved = (updated: Member) => {
    setMembers((previous) =>
      previous ? previous.map((member) => (member.userId === updated.userId ? updated : member)) : previous,
    );
    focusRecord.current = updated.userId;
    void refresh();
    say("ok", t("Role updated for {name}.").replace("{name}", () => displayName(updated, t)));
  };

  const memberRemoved = (removed: Member) => {
    setMembers((previous) => (previous ? previous.filter((member) => member.userId !== removed.userId) : previous));
    focusRecord.current = null;
    focusInvite.current = true;
    void refresh();
    say("ok", t("{name} removed.").replace("{name}", () => displayName(removed, t)));
  };

  const inviteSent = (invite: Invite) => {
    setInvites((previous) =>
      previous ? [...previous.filter((row) => row.email !== invite.email), invite] : previous,
    );
    void refresh();
    say("ok", t("Invite sent."));
  };

  const inviteResent = (invite: Invite) => {
    setInvites((previous) =>
      previous ? [...previous.filter((row) => row.email !== invite.email), invite] : previous,
    );
    void refresh();
    say("ok", t("Invite sent again."));
  };

  /**
   * The board keeps a refused or unconfirmed invite pending and stamped, so a
   * reload shows the same flagged row to every owner. Refresh, then say why.
   */
  const inviteUndelivered = (failed: boolean) => {
    void refresh();
    say("warn", failed ? t("The invitation email was refused. Nothing was sent — try again.") : t("We couldn’t confirm the invitation email was sent."));
  };

  const inviteRevoked = (invite: Invite) => {
    setInvites((previous) => (previous ? previous.filter((row) => row.id !== invite.id) : previous));
    focusInvite.current = true;
    void refresh();
    say("ok", t("Invite revoked."));
  };

  const opened = open?.drawer ?? null;
  const openMember =
    opened?.kind === "member" ? members?.find((member) => member.userId === opened.id) ?? null : null;
  const openInvite =
    opened?.kind === "invite" ? invites?.find((invite) => invite.id === opened.id) ?? null : null;

  const memberRow = (member: Member) => {
    const name = displayName(member, t);
    const line = [member.email, roleName(member.role, t)].filter((part): part is string => Boolean(part)).join(" · ");
    const body = (
      <>
        <span className="members-avatar" aria-hidden="true">
          {initials(member.name, member.email)}
        </span>
        <span className="settings-row-text">
          <b>{name}</b>
          <small>{line}</small>
        </span>
      </>
    );
    return canManage ? (
      <button
        type="button"
        className="settings-row"
        data-record-id={member.userId}
        onClick={() => showDrawer({ kind: "member", id: member.userId })}
      >
        {body}
      </button>
    ) : (
      <div className="settings-row">{body}</div>
    );
  };

  return (
    <section className="members-page" aria-labelledby="members-title" aria-busy={loading || undefined}>
      <header className="members-heading">
        <div>
          <h1 id="members-title">{t("Members")}</h1>
          <p>{t("People in this workspace.")}</p>
        </div>
        {canManage && members !== null && (
          <button
            ref={inviteButton}
            type="button"
            className="settings-quiet members-invite"
            onClick={() => showDrawer({ kind: "newInvite" })}
          >
            <span aria-hidden="true">+</span>
            {t("Invite")}
          </button>
        )}
      </header>

      {viewer.operator === null ? (
        <PageState title={viewer.access === "unavailable" ? t("Access could not be confirmed") : t("Checking access…")}>
          {viewer.access === "unavailable" ? (
            <>
              {t("The board's access check did not answer, so members stay closed.")}{" "}
              {viewer.onRetryAccess && (
                <Button size="sm" onClick={viewer.onRetryAccess}>
                  {t("Retry access check")}
                </Button>
              )}
            </>
          ) : (
            t("Checking whether this session may manage members…")
          )}
        </PageState>
      ) : loading && members === null ? (
        <div className="settings-rows" role="status" aria-label={t("Loading members…")}>
          {[0, 1, 2, 3].map((row) => (
            <div className="settings-row" key={row} aria-hidden="true">
              <span className="members-skel members-skel-avatar" />
              <span className="settings-row-text">
                <span className="members-skel" style={{ width: `${40 - row * 5}%` }} />
                <span className="members-skel" style={{ width: `${58 - row * 5}%` }} />
              </span>
            </div>
          ))}
        </div>
      ) : failed && members === null ? (
        <div className="members-error" role="alert">
          <span>{t("Couldn’t load members.")}</span>
          <Button size="sm" onClick={() => setAttempt((count) => count + 1)}>
            {t("Retry")}
          </Button>
        </div>
      ) : (
        <>
          <div className="members-search-row">
            <input
              ref={searchField}
              className="field members-search"
              type="search"
              value={query}
              placeholder={t("Search members")}
              aria-label={t("Search members")}
              onChange={(event) => setQuery(event.target.value)}
            />
          </div>

          {failed && (
            <div className="members-error" role="alert">
              <span>{t("Couldn’t refresh this list, so it may be out of date.")}</span>
              <Button size="sm" onClick={() => setAttempt((count) => count + 1)}>
                {t("Retry")}
              </Button>
            </div>
          )}

          {members !== null && members.length === 0 && (
            <div className="settings-rows">
              <div className="members-empty">
                <p className="members-empty-title">{t("No members yet.")}</p>
              </div>
            </div>
          )}

          {visible.length > 0 && (
            <ul className="settings-rows" aria-label={t("Members")}>
              {visible.map((member) => (
                <li key={member.userId}>{memberRow(member)}</li>
              ))}
            </ul>
          )}

          {members !== null && members.length > 0 && visible.length === 0 && (
            <div className="settings-rows">
              <div className="members-empty">
                <p className="members-empty-title">
                  {t("No one matches “{query}”").replace("{query}", () => query.trim())}
                </p>
                <button
                  type="button"
                  className="settings-quiet members-clear"
                  onClick={() => {
                    setQuery("");
                    searchField.current?.focus();
                  }}
                >
                  {t("Clear search")}
                </button>
              </div>
            </div>
          )}

          {canManage && invites !== null && invites.length > 0 && (
            <section className="members-group" aria-labelledby="members-pending">
              <p className="members-group-label" id="members-pending">
                {t("Pending invites")} <span className="members-count">{invites.length}</span>
              </p>
              <ul className="settings-rows" aria-label={t("Pending invites")}>
                {invites.map((invite) => (
                  <li key={invite.id}>
                    <button
                      type="button"
                      className="settings-row"
                      data-record-id={invite.id}
                      onClick={() => showDrawer({ kind: "invite", id: invite.id })}
                    >
                      <span className="settings-row-text">
                        <b>{invite.email}</b>
                        <small>{roleLine(invite.role, expiryText(invite.expiresAt, formatDate), t)}</small>
                      </span>
                      {invite.emailStatus && (
                        <span className="chip bg-warn/10 text-warn">
                          {invite.emailStatus === "failed" ? t("Not sent") : t("May not have arrived")}
                        </span>
                      )}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {canManage && members !== null && members.length === 1 && invites !== null && invites.length === 0 && !query.trim() && (
            <p className="members-hint">
              {t("It’s just you so far. Invited people don’t need to install anything.")}
            </p>
          )}

        </>
      )}

      {canManage && openMember && (
        <MemberDrawer
          key={open?.token}
          member={openMember}
          soleOwner={soleOwner(openMember)}
          onClose={() => closeDrawer(open?.token)}
          onSaved={memberSaved}
          onRemoved={memberRemoved}
        />
      )}

      {canManage && openInvite && (
        <InviteDrawer
          key={open?.token}
          invite={openInvite}
          onClose={() => closeDrawer(open?.token)}
          onRevoked={inviteRevoked}
          onResent={inviteResent}
          onUndelivered={inviteUndelivered}
          onRefresh={() => void refresh()}
        />
      )}

      {canManage && open?.drawer.kind === "newInvite" && (
        <NewInviteDrawer
          key={open.token}
          onClose={() => closeDrawer(open.token)}
          onSent={inviteSent}
          onUndelivered={inviteUndelivered}
        />
      )}

      <Toast msg={toast} />
    </section>
  );
}

function MemberDrawer({
  member,
  soleOwner,
  onClose,
  onSaved,
  onRemoved,
}: {
  member: Member;
  soleOwner: boolean;
  onClose: () => void;
  onSaved: (member: Member) => void;
  onRemoved: (member: Member) => void;
}) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  const [role, setRole] = useState<Role>(member.role);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const name = displayName(member, t);
  const dirty = role !== member.role;
  const joined = joinedText(member.joinedAt, formatDate);

  useConfirmEscape(confirming, () => setConfirming(false));

  const save = async () => {
    if (!dirty || busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await boardWrite("/api/team/members/role", { user_id: member.userId, role });
      if (!isRecord(result)) throw new Error("unexpected role answer");
      const updated = parseMember(result.member);
      onSaved(updated);
      onClose();
    } catch (failure) {
      setError(roleFailure(failure, t));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await boardWrite("/api/team/members/remove", { user_id: member.userId });
      // The board names who it removed; anything else is not this removal.
      if (!isRecord(result) || result.removed !== member.userId)
        throw new Error("unexpected removal answer");
      onRemoved(member);
      onClose();
    } catch (failure) {
      setError(removeFailure(failure, t));
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <div className="members-drawer">
      <dl className="members-dl">
        <dt>{t("Email")}</dt>
        <dd>{member.email ?? t("Not recorded")}</dd>
        <dt>{t("Joined")}</dt>
        <dd>
          {joined ? <time dateTime={member.joinedAt}>{joined}</time> : t("Not recorded")}
        </dd>
      </dl>

      {soleOwner ? (
        <div className="members-field">
          <p className="slabel">{t("Role")}</p>
          <p className="members-role-text">{t("Owner")}</p>
          <p className="members-field-note">{t("The only owner. Make someone else an owner before changing this.")}</p>
        </div>
      ) : (
        <div className="members-field">
          <label className="members-field-label" htmlFor="member-role">
            {t("Role")}
          </label>
          <Select
            id="member-role"
            full
            value={role}
            disabled={busy}
            aria-label={t("Role")}
            options={[
              { value: "member", label: t("Member: works on the board") },
              { value: "owner", label: t("Owner: also manages members and settings") },
            ]}
            onChange={(value) => setRole(value as Role)}
          />
        </div>
      )}

      {error && (
        <p className="members-field-error" role="alert">
          {error}
        </p>
      )}

      {!soleOwner && !confirming && (
        <button type="button" className="members-danger" onClick={() => setConfirming(true)}>
          {t("Remove from workspace")}
        </button>
      )}
    </div>
  );

  const foot = confirming ? (
    <div className="members-confirm">
      <span className="members-confirm-text">
        {t("Remove {name}? They lose access straight away. Their work stays on the board.").replace(
          "{name}",
          () => name,
        )}
      </span>
      <div className="members-confirm-actions">
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => setConfirming(false)}>
          {t("Cancel")}
        </Button>
        <Button variant="danger" size="sm" loading={busy} onClick={() => void remove()}>
          {t("Remove")}
        </Button>
      </div>
    </div>
  ) : dirty ? (
    <div className="members-foot-actions">
      <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>
        {t("Discard")}
      </Button>
      <Button variant="primary" size="sm" loading={busy} onClick={() => void save()}>
        {busy ? t("Saving…") : t("Save")}
      </Button>
    </div>
  ) : null;

  return (
    <DrawerShell
      kind="member"
      label={name}
      title={name}
      avatar={initials(member.name, member.email)}
      tabs={[{ id: "details", label: name, panel: body }]}
      foot={foot}
      onClose={onClose}
    />
  );
}

function InviteDrawer({
  invite,
  onClose,
  onRevoked,
  onResent,
  onUndelivered,
  onRefresh,
}: {
  invite: Invite;
  onClose: () => void;
  onRevoked: (invite: Invite) => void;
  onResent: (invite: Invite) => void;
  onUndelivered: (failed: boolean) => void;
  onRefresh: () => void;
}) {
  const t = useTranslate();
  const { formatDate } = useLocale();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const expires = expiryText(invite.expiresAt, formatDate);

  useConfirmEscape(confirming, () => setConfirming(false));

  const resend = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await boardWrite("/api/team/invites/resend", { invite_id: invite.id });
      const resent = parseInvitePayload(result);
      // The board stamps the outcome it recorded; a flagged row is not "sent".
      if (resent.emailStatus) {
        onUndelivered(resent.emailStatus === "failed");
        onClose();
        return;
      }
      onResent(resent);
      onClose();
    } catch (failure) {
      const code = failure instanceof ApiError ? failure.code : null;
      if (code === "invite_email_uncertain" || code === "invite_email_failed") {
        onUndelivered(code === "invite_email_failed");
        onClose();
        return;
      }
      // A refused resend retires the old link, so the row behind must reload.
      onRefresh();
      setError(inviteFailure(failure, t));
    } finally {
      setBusy(false);
    }
  };

  const revoke = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await boardWrite("/api/team/invites/revoke", { invite_id: invite.id });
      // The board answers with the invite it retired; nothing else is a revoke.
      const receipt = isRecord(result) ? result.invite : undefined;
      if (!isRecord(receipt) || receipt.status !== "revoked")
        throw new Error("unexpected revoke answer");
      onRevoked(invite);
      onClose();
    } catch (failure) {
      setError(revokeFailure(failure, t));
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <div className="members-drawer">
      <p className="members-lead">
        {invite.emailStatus === "failed"
          ? t("The invitation email was refused, so nothing reached them. Send it again.")
          : invite.emailStatus === "uncertain"
            ? t("The invite email may not have arrived. You can invite them again.")
            : t("They haven’t joined yet.")}
      </p>
      <dl className="members-dl">
        <dt>{t("Role")}</dt>
        <dd>{roleName(invite.role, t)}</dd>
        <dt>{t("Expires")}</dt>
        <dd>
          {expires && <time dateTime={invite.expiresAt}>{expires}</time>}
          {!expires && t("Not recorded")}
        </dd>
      </dl>

      {error && (
        <p className="members-field-error" role="alert">
          {error}
        </p>
      )}

      {!confirming && (
        <button type="button" className="members-danger" onClick={() => setConfirming(true)}>
          {t("Revoke invite")}
        </button>
      )}
    </div>
  );

  const foot = confirming ? (
    <div className="members-confirm">
      <span className="members-confirm-text">{t("Revoke this invite? The link stops working.")}</span>
      <div className="members-confirm-actions">
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => setConfirming(false)}>
          {t("Cancel")}
        </Button>
        <Button variant="danger" size="sm" loading={busy} onClick={() => void revoke()}>
          {t("Revoke")}
        </Button>
      </div>
    </div>
  ) : invite.emailStatus ? (
    <div className="members-foot-actions">
      <Button variant="primary" size="sm" loading={busy} onClick={() => void resend()}>
        {busy ? t("Sending…") : t("Invite again")}
      </Button>
    </div>
  ) : null;

  return (
    <DrawerShell
      kind="invite"
      label={invite.email}
      title={invite.email}
      tabs={[{ id: "details", label: invite.email, panel: body }]}
      foot={foot}
      onClose={onClose}
    />
  );
}

function NewInviteDrawer({
  onClose,
  onSent,
  onUndelivered,
}: {
  onClose: () => void;
  onSent: (invite: Invite) => void;
  onUndelivered: (failed: boolean) => void;
}) {
  const t = useTranslate();
  const fieldId = useId();
  const errorId = `${fieldId}-error`;
  const emailRef = useRef<HTMLInputElement | null>(null);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("member");
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => emailRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, []);

  const send = async () => {
    if (busy) return;
    const address = email.trim().toLowerCase();
    if (!EMAIL.test(address)) {
      setFieldError(t("Enter a valid email address."));
      emailRef.current?.focus();
      return;
    }
    setFieldError(null);
    setBusy(true);
    try {
      const result = await boardWrite("/api/team/invites", { email: address, role });
      const created = parseInvitePayload(result);
      // The board stamps what it recorded; a flagged invite is not "sent".
      if (created.emailStatus) {
        onUndelivered(created.emailStatus === "failed");
        onClose();
        return;
      }
      onSent(created);
      onClose();
    } catch (failure) {
      const code = failure instanceof ApiError ? failure.code : undefined;
      const field = inviteFieldFailure(code, t);
      if (field) {
        setFieldError(field);
        emailRef.current?.focus();
        return;
      }
      // Any other outcome may have committed a flagged invite, so read the
      // board's own list instead of guessing at it here.
      onUndelivered(code === "invite_email_failed");
      onClose();
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <div className="members-drawer">
      <p className="members-lead">
        {t("They’ll get an email with a link that lasts 7 days, and don’t need to install anything.")}
      </p>

      <div className="members-field">
        <label className="members-field-label" htmlFor={fieldId}>
          {t("Work email")}
        </label>
        <input
          ref={emailRef}
          id={fieldId}
          className="field"
          type="email"
          inputMode="email"
          autoComplete="off"
          value={email}
          disabled={busy}
          aria-invalid={fieldError ? true : undefined}
          aria-describedby={fieldError ? errorId : undefined}
          onChange={(event) => {
            setEmail(event.target.value);
            if (fieldError) setFieldError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void send();
            }
          }}
        />
        {fieldError && (
          <p id={errorId} className="members-field-error" role="alert">
            {fieldError}
          </p>
        )}
      </div>

      <div className="members-field">
        <label className="members-field-label" htmlFor={`${fieldId}-role`}>
          {t("Role")}
        </label>
        <Select
          id={`${fieldId}-role`}
          full
          value={role}
          disabled={busy}
          aria-label={t("Role")}
          options={[
            { value: "member", label: t("Member: works on the board") },
            { value: "owner", label: t("Owner: also manages members and settings") },
          ]}
          onChange={(value) => setRole(value as Role)}
        />
      </div>
    </div>
  );

  return (
    <DrawerShell
      kind="invite-new"
      label={t("Invite member")}
      title={t("Invite member")}
      tabs={[{ id: "details", label: t("Invite member"), panel: body }]}
      foot={
        <div className="members-foot-actions">
          <Button variant="primary" size="sm" loading={busy} onClick={() => void send()}>
            {busy ? t("Sending…") : t("Send invite")}
          </Button>
        </div>
      }
      onClose={onClose}
    />
  );
}
