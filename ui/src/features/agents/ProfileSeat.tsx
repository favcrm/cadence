import { useTranslate } from "../../lib/locale";
import type { Agent, HiredProfile } from "../../lib/types";
import AgentAvatar from "./AgentAvatar";
import { agentLocation } from "./agentView";

/** How a runtime attaches to a profile, and how it reads the profile back. */
export function ConnectRuntime({ profile }: { profile: HiredProfile }) {
  const t = useTranslate();
  return (
    <div className="rc-connect">
      <b>{t("Connect a runtime")}</b>
      <small>{t("Read the profile:")}</small>
      <code>{`cadence agent context ${profile.id}`}</code>
      <small>{t("Register a runtime against it:")}</small>
      <code>{`cadence join <group> <provider> --alias <runtime-name> --profile ${profile.id}`}</code>
    </div>
  );
}

/** A profile as a seat: with no runtime it is "Idle · no runtime" and shows how
 *  to connect one; with runtimes it says where each runs and on which model. */
export default function ProfileSeat({
  profile,
  roleTitle,
  runtimes,
}: {
  profile: HiredProfile;
  roleTitle: string;
  runtimes: Agent[];
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
        {runtimes.length === 0 ? (
          <>
            <div>{t("Idle · no runtime")}</div>
            <ConnectRuntime profile={profile} />
          </>
        ) : (
          runtimes.map((rt) => (
            <div key={rt.alias}>
              {rt.alias} · {agentLocation(rt) === "cloud" ? t("cloud") : t("this computer")} ·{" "}
              {t("Model:")} {rt.model ?? profile.model}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
