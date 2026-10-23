import { useContext, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { navigate } from "../../lib/useLocation";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import { HireBlock } from "../auth/WriteGate";
import type { ThreadEntry } from "../home/thread";
import type { AgentRole } from "../../lib/types";
import HireSheet from "./HireSheet";
import { HireGate, RoleCard } from "./RecruitCards";
import { nextAlias, useAgentRoles } from "./agentRoles";

/** Master's proposal in the thread (CAD-1752): the role card, a short line
 *  of its reasoning, and two buttons. Review and hire opens the hire sheet
 *  prefilled with the proposed name and job; nothing is registered until the
 *  operator presses Hire there. */
export default function HireProposalCard({
  entry,
  reason,
}: {
  entry: ThreadEntry;
  reason: string | null;
}) {
  const t = useTranslate();
  const writeBlock = useContext(HireBlock);
  const roles = useAgentRoles();
  const roster = useQuery(resources.agents);
  const [open, setOpen] = useState(false);
  const [hired, setHired] = useState<string | null>(null);
  const proposal = entry.payload ?? {};
  const roleId = typeof proposal.role === "string" ? proposal.role : "";
  const role: AgentRole | undefined = roles.data?.roles.find((r) => r.id === roleId);
  const taken = (roster.data?.agents ?? []).map((a) => a.alias);
  const name = typeof proposal.name === "string" && proposal.name ? proposal.name : null;
  const job = typeof proposal.job === "string" ? proposal.job : undefined;

  if (!role || !roles.data) {
    return (
      <p className="text-micro text-ink-500" data-kind="hire-proposal">
        · {roles.error ? t("The proposed role could not be loaded.") : entry.text}
      </p>
    );
  }
  return (
    <div className="rc-proposal" data-kind="hire-proposal">
      {reason && <p className="rc-proposal-why">{reason}</p>}
      <RoleCard role={role} payload={roles.data} onPick={() => setOpen(true)} />
      <HireGate reason={writeBlock} />
      {hired ? (
        <p className="ap-saved" role="status">
          {t("Hired {name}.").replace("{name}", () => hired)}{" "}
          <Button variant="ghost" size="sm" href={`/agents/${encodeURIComponent(hired)}`}>
            {t("Open")}
          </Button>
        </p>
      ) : (
        <div className="rc-sugs">
          <Button
            variant="primary"
            size="sm"
            disabled={writeBlock !== null}
            title={writeBlock ?? t("Opens the hire sheet; nothing is hired until you press Hire")}
            onClick={() => setOpen(true)}
          >
            {t("Review and hire")}
          </Button>
          <Button variant="ghost" size="sm" onClick={() => navigate("/agents?recruit=1")}>
            {t("See other roles")}
          </Button>
        </div>
      )}
      {open && (
        <HireSheet
          role={role}
          payload={roles.data}
          alias={name ?? nextAlias(role.id, taken)}
          job={job}
          taken={taken}
          onClose={() => setOpen(false)}
          onHired={(alias) => {
            setOpen(false);
            setHired(alias);
            void resources.agents.refresh();
          }}
        />
      )}
    </div>
  );
}
