import { useEffect, useRef } from "react";
import { useTranslate } from "../../lib/locale";
import type { HiredProfile } from "../../lib/types";
import Button from "../../ui/Button";
import AgentAvatar from "./AgentAvatar";
import type { ProfileWork } from "./agentView";

/** The providers `cadence join` takes. */
const PROVIDERS = "claude, codex, pi, devin or cursor";

/** How a runtime attaches to a profile and reads it back, as commands that run
 *  as printed. A native board joins a runtime under a group (a pm agent); a
 *  hosted board has no `join` and starts a session on an issue instead. */
export function ConnectRuntime({
  profile,
  hosted,
  groups,
}: {
  profile: HiredProfile;
  hosted: boolean;
  groups: string[];
}) {
  const t = useTranslate();
  return (
    <div className="rc-connect">
      <b>{t("Connect a runtime")}</b>
      <small>{t("Read the profile:")}</small>
      <code>{`cadence agent context ${profile.id}`}</code>
      {hosted ? (
        <>
          <small>{t("Start a session on an issue under this profile:")}</small>
          <code>{`cadence issue start <issue-id> --repo <path-to-checkout> --profile ${profile.id}`}</code>
        </>
      ) : groups.length > 0 ? (
        <>
          <small>{t("Register a runtime against it (provider: {providers}):").replace("{providers}", PROVIDERS)}</small>
          <code>{`cadence join ${groups[0]} <provider> --profile ${profile.id}`}</code>
        </>
      ) : (
        <>
          <small>{t("A runtime joins a group. Create one first:")}</small>
          <code>cadence agent register lead --provider inbox --role pm</code>
          <small>{t("Then register a runtime against the profile (provider: {providers}):").replace("{providers}", PROVIDERS)}</small>
          <code>{`cadence join lead <provider> --profile ${profile.id}`}</code>
        </>
      )}
    </div>
  );
}

function workText(work: ProfileWork, t: (english: string) => string): string {
  const line = `${work.issue.id} · ${work.issue.title}`;
  return work.open ? line : t("Last: {issue}").replace("{issue}", () => line);
}

/** A profile with no runtime, in a seat card's shape: avatar, name, role, then
 *  one status line. How to connect a runtime lives in its details. A profile
 *  with runtimes is carried by their own seats. */
export default function ProfileSeat({
  profile,
  roleTitle,
  work,
  onOpen,
}: {
  profile: HiredProfile;
  roleTitle: string;
  work: ProfileWork | null;
  onOpen: () => void;
}) {
  const t = useTranslate();
  const status = work ? workText(work, t) : t("Idle · no runtime");
  return (
    <button
      type="button"
      className="rc-seat clickable"
      aria-label={t("Open agent {alias}").replace("{alias}", () => profile.alias)}
      onClick={onOpen}
    >
      <AgentAvatar slug="custom" look={profile.look ?? undefined} size={44} still />
      <span className="min-w-0">
        <span className="t">{profile.alias}</span>
        <span className="s">{roleTitle}</span>
      </span>
      <span className="st">
        <span className="rc-dot" style={{ "--c": "var(--color-ink-500)" } as React.CSSProperties} />
        <span className="min-w-0 truncate" title={status}>
          {status}
        </span>
      </span>
    </button>
  );
}

/** A profile's details: a right drawer on desktop, a bottom sheet on phones,
 *  like the hire sheet it follows. Focus returns and Esc closes. */
export function ProfileSheet({
  profile,
  roleTitle,
  work,
  hosted,
  groups,
  onClose,
  onOpenIssue,
}: {
  profile: HiredProfile;
  roleTitle: string;
  work: ProfileWork | null;
  hosted: boolean;
  groups: string[];
  onClose: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    const trigger = document.activeElement;
    const overflow = document.body.style.overflow;
    dialog?.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      dialog?.close();
      document.body.style.overflow = overflow;
      if (trigger instanceof HTMLElement && trigger.isConnected) trigger.focus();
    };
  }, []);

  return (
    <dialog
      ref={dialogRef}
      className="rc-sheet bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby="profile-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="rc-sheet-h">
        <AgentAvatar slug="custom" look={profile.look ?? undefined} size={52} still />
        <div className="min-w-0">
          <div className="t" id="profile-title">
            {profile.alias}
          </div>
          <div className="s">{roleTitle}</div>
        </div>
        <Button variant="ghost" size="sm" className="x" aria-label={t("Close")} onClick={onClose}>
          ✕
        </Button>
      </div>
      <div className="rc-sheet-b">
        <div className="rc-blk">
          <h4>{t("Status")}</h4>
          <p className="rc-profile-line">{t("Idle · no runtime")}</p>
        </div>
        {work && (
          <div className="rc-blk">
            <h4>{t("Latest work")}</h4>
            <button type="button" className="rc-profile-work" onClick={() => onOpenIssue(work.issue.id)}>
              {workText(work, t)}
            </button>
          </div>
        )}
        {profile.job && (
          <div className="rc-blk">
            <h4>{t("Job")}</h4>
            <p className="rc-profile-job">{profile.job}</p>
          </div>
        )}
        <div className="rc-blk">
          <h4>{t("Model")}</h4>
          <p className="rc-profile-line">{profile.model === "inherit" ? t("Inherit from the runtime") : profile.model}</p>
        </div>
        <ConnectRuntime profile={profile} hosted={hosted} groups={groups} />
      </div>
    </dialog>
  );
}
