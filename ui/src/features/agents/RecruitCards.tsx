import type { ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import type { AgentRole, AgentRolesPayload } from "../../lib/types";
import AgentAvatar from "./AgentAvatar";
import { OPERATOR_HIRE_REASON } from "../auth/gate";
import { categoryLabel } from "./agentRoles";

/** The banner shown to a viewer who cannot hire: the gate's own reason
 *  (operator-only, read-only, still checking), never fixed text. */
export function HireGate({ reason }: { reason: string | null }) {
  const t = useTranslate();
  if (reason === null) return null;
  return (
    <div className="rc-gate" role="status">
      <span aria-hidden="true">🔒</span>
      <div>
        <b>{t(reason)}</b>
        {reason === OPERATOR_HIRE_REASON && (
          <> {t("You can browse the catalogue and ask the operator to recruit one.")}</>
        )}
      </div>
    </div>
  );
}

/** One catalogue card: the pinned look, title, category, job line and kind. */
export function RoleCard({
  role,
  payload,
  onTeam = 0,
  onPick,
}: {
  role: AgentRole;
  payload: AgentRolesPayload;
  onTeam?: number;
  onPick: (role: AgentRole) => void;
}) {
  const t = useTranslate();
  // TODO(CAD-1753): the real per-ticket sandbox entitlement; until then the
  // badge follows the hosted flag only.
  const sandboxReady = payload.install.hosted && role.kind === "tickets";
  return (
    <button type="button" className="rc-role" onClick={() => onPick(role)}>
      <div className="top">
        <AgentAvatar slug="custom" look={role.look} size={40} />
        <div>
          <div className="t">{role.title}</div>
          <div className="k">{categoryLabel(payload, role.category)}</div>
        </div>
        {onTeam > 0 && (
          <span className="rc-hired-n">{t("{count} on team").replace("{count}", String(onTeam))}</span>
        )}
      </div>
      <div className="d">{role.job}</div>
      <div className="f">
        <span className="rc-badge">
          {role.kind === "tickets" ? t("works on tickets") : t("works in your apps")}
        </span>
        {sandboxReady && <span className="rc-badge">{t("☁ sandbox-ready")}</span>}
      </div>
    </button>
  );
}

/** A dashed "something else" card (Ask Master, Start blank). */
export function GhostCard({
  look,
  name,
  title,
  kicker,
  text,
  onClick,
}: {
  look: string;
  name: string;
  title: string;
  kicker: string;
  text: ReactNode;
  onClick: () => void;
}) {
  return (
    <button type="button" className="rc-role ghost" onClick={onClick} aria-label={title}>
      <div className="top">
        <AgentAvatar slug="custom" look={look} size={40} title={name} />
        <div>
          <div className="t">{title}</div>
          <div className="k">{kicker}</div>
        </div>
      </div>
      <div className="d">{text}</div>
    </button>
  );
}
