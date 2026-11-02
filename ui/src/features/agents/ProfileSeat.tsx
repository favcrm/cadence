import { useTranslate } from "../../lib/locale";
import type { HiredProfile } from "../../lib/types";
import AgentAvatar from "./AgentAvatar";

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

/** A profile with no runtime: "Idle · no runtime", and how to connect one. A
 *  profile with runtimes is carried by their own seats. */
export default function ProfileSeat({
  profile,
  roleTitle,
  hosted,
  groups,
}: {
  profile: HiredProfile;
  roleTitle: string;
  hosted: boolean;
  groups: string[];
}) {
  const t = useTranslate();
  return (
    <div className="rc-seat" aria-label={t("Profile {alias}").replace("{alias}", () => profile.alias)}>
      <AgentAvatar slug="custom" look={profile.look ?? undefined} size={44} still />
      <div className="min-w-0">
        <div className="t">{profile.alias}</div>
        <div className="s">{roleTitle}</div>
      </div>
      <div className="st" style={{ display: "block" }}>
        <div>{t("Idle · no runtime")}</div>
        <ConnectRuntime profile={profile} hosted={hosted} groups={groups} />
      </div>
    </div>
  );
}
