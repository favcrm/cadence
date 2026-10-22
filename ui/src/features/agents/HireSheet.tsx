import { useContext, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import Button from "../../ui/Button";
import { WriteGate } from "../auth/WriteGate";
import type { AgentRole, AgentRolesPayload } from "../../lib/types";
import AgentAvatar from "./AgentAvatar";
import { HireGate } from "./RecruitCards";
import { aliasProblem, categoryLabel, modelSourceText, runsOnText } from "./agentRoles";
import "./recruit.css";

function Chip({ text, planned = false }: { text: string; planned?: boolean }) {
  const t = useTranslate();
  return (
    <span className={`rc-kst${planned ? " planned" : ""}`}>
      {t(text)}
      {planned ? ` · ${t("planned")}` : ""}
    </span>
  );
}

/** The hire sheet: a right drawer on desktop, a bottom sheet on phones. A
 *  modal dialog like the other drawers - focus returns, Esc closes. */
export default function HireSheet({
  role,
  payload,
  alias: suggested,
  taken,
  onClose,
  onHired,
}: {
  role: AgentRole;
  payload: AgentRolesPayload;
  alias: string;
  taken: string[];
  onClose: () => void;
  onHired: (alias: string, role: AgentRole) => void;
}) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [alias, setAlias] = useState(suggested);
  const [job, setJob] = useState(role.instructions);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const hosted = payload.install.hosted;
  const modelError = "error" in role.model ? role.model.error : null;
  const problem = aliasProblem(alias, taken);
  const blocked = writeBlock !== null || modelError !== null || problem !== null;

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

  const hire = async () => {
    if (blocked || saving) return;
    setSaving(true);
    setErr(null);
    try {
      const name = alias.trim();
      await api.registerAgent({
        alias: name,
        catalogue: role.id,
        // The role's job is the default; only an edited job is sent.
        instructions: job !== role.instructions ? job : undefined,
      });
      onHired(name, role);
    } catch (error) {
      setErr(error instanceof ApiError || error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  const why = writeBlock !== null ? t("Only the operator can hire.") : t("Nothing runs until you hire.");
  return (
    <dialog
      ref={dialogRef}
      className="rc-sheet bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby="hire-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="rc-sheet-h">
        <AgentAvatar slug="custom" look={role.look} size={52} active />
        <div>
          <div className="t" id="hire-title">
            {role.title}
          </div>
          <div className="s">
            {t(categoryLabel(payload, role.category))} · {t("built-in role")}
          </div>
        </div>
        <Button
          variant="ghost"
          size="sm"
          className="x"
          icon={<IconClose />}
          aria-label={t("Close")}
          onClick={onClose}
        />
      </div>
      <div className="rc-sheet-b">
        <HireGate reason={writeBlock} />
        <div className="rc-blk">
          <h4>{t("What it does")}</h4>
          <ul className="rc-does">
            {role.does.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </div>
        <label className="rc-fld">
          <span className="lab">{t("Name")}</span>
          <input
            className="field"
            value={alias}
            aria-invalid={problem !== null}
            onChange={(event) => setAlias(event.target.value)}
          />
          {problem ? (
            <span className="bad" role="alert">
              {t(problem)}
            </span>
          ) : (
            <span className="hint">{t("How you will mention it: @{name}").replace("{name}", () => alias.trim())}</span>
          )}
        </label>

        <div className="rc-runs">
          <span className="ri" aria-hidden="true">
            {hosted ? "☁" : "⌂"}
          </span>
          <div>
            <b>{t("Runs on {where}").replace("{where}", () => t(runsOnText(hosted)))}</b>
            <small>
              {hosted
                ? t("Always on, through your workspace's model gateway")
                : t("Uses this computer and your model settings")}
            </small>
            {/* TODO(CAD-1753): gate on the real sandbox entitlement; for now the hosted flag. */}
            {role.kind === "tickets" && hosted && (
              <small className="sb">
                {t("Ticket work can run in a cloud sandbox — chosen per ticket, not here.")}
              </small>
            )}
          </div>
        </div>

        <div className="rc-blk">
          <h4>{t("What it knows")}</h4>
          <div className="rc-kst-row">
            <Chip text="Its job (below)" />
            <Chip text="Workspace wiki" />
            <Chip text="Project notes of the ticket it works on" />
            <Chip text="Its own memory" planned />
          </div>
        </div>
        <div className="rc-blk">
          <h4>{t("Skills")}</h4>
          <div className="rc-kst-row">
            {role.skills.map((skill) => (
              <span key={skill} className="rc-kst">
                {skill}
              </span>
            ))}
          </div>
          <p className="rc-note">
            {t(
              "Today every agent on the machine shares all bundled skills; per-role skill sets are planned.",
            )}
          </p>
        </div>
        <div className="rc-blk">
          <h4>{t("Tools")}</h4>
          <div className="rc-kst-row">
            {role.tools.map((tool, index) => (
              <Chip key={tool} text={tool} planned={role.kind === "apps" && index === 0} />
            ))}
            <Chip text="Asks you before sending or publishing" planned />
          </div>
          <p className="rc-note">{t("Not copied: logins, API keys, MCP servers. Connect them after hiring.")}</p>
        </div>

        {modelError === null ? (
          <div className="rc-modelrow">
            {t("Model:")} <b>{"id" in role.model ? role.model.id : ""}</b>
            <span>
              · {modelSourceText(role)} · {t("set in Settings › Agents")}
            </span>
          </div>
        ) : (
          <div className="rc-modelrow bad" role="alert">
            {t("No model can be chosen for this role:")} {modelError}
          </div>
        )}
        <details className="rc-adv">
          <summary>{t("Customize job and advanced")}</summary>
          <label className="rc-fld">
            <span className="lab">{t("Job (its standing instructions)")}</span>
            <textarea
              className="field rc-text"
              value={job}
              onChange={(event) => setJob(event.target.value)}
            />
          </label>
          <dl className="rc-kv" style={{ marginTop: "0.75rem" }}>
            <dt>{t("team role")}</dt>
            <dd>{role.team_role}</dd>
            <dt>{t("runtime")}</dt>
            <dd>
              {payload.install.provider} · {hosted ? t("agenticos gateway") : t("local")}
            </dd>
            <dt>{t("catalogue")}</dt>
            <dd>
              {role.id} {t("(recorded on the agent)")}
            </dd>
            <dt>{t("look")}</dt>
            <dd>{role.look}</dd>
          </dl>
        </details>
        {err && (
          <p role="alert" className="text-label text-fail">
            {err}
          </p>
        )}
      </div>
      <div className="rc-sheet-f">
        <span className="why">{why}</span>
        <Button variant="ghost" size="sm" onClick={onClose}>
          {t("Cancel")}
        </Button>
        <Button
          variant="primary"
          disabled={blocked || saving}
          title={writeBlock ?? modelError ?? undefined}
          onClick={() => void hire()}
        >
          {saving ? t("Hiring…") : t("Hire {name}").replace("{name}", () => alias.trim())}
        </Button>
      </div>
    </dialog>
  );
}
