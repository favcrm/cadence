import { useContext, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { HireBlock } from "../auth/WriteGate";
import type { AgentProfile, AgentRole, AgentRolesPayload } from "../../lib/types";
import AgentAvatar from "./AgentAvatar";
import { HireGate } from "./RecruitCards";
import { aliasProblem, categoryLabel } from "./agentRoles";

/** The model a profile starts with: the runtime decides at launch. */
const INHERIT = "inherit";

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
  job: startJob,
  taken,
  onClose,
  onHired,
}: {
  role: AgentRole;
  payload: AgentRolesPayload;
  alias: string;
  /** A prefilled job (a proposal's); the role's own when absent. */
  job?: string;
  taken: string[];
  onClose: () => void;
  onHired: (profile: AgentProfile, role: AgentRole) => void;
}) {
  const t = useTranslate();
  const hireBlock = useContext(HireBlock);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [alias, setAlias] = useState(suggested);
  const [job, setJob] = useState(startJob ?? role.instructions);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [model, setModel] = useState(INHERIT);
  const [models, setModels] = useState<string[]>([]);
  const problem = aliasProblem(alias, taken);
  const blocked = hireBlock !== null || problem !== null;

  // The registered models are an optional choice: a failed read leaves the
  // sheet on "Inherit from the runtime", which never blocks a hire.
  useEffect(() => {
    let live = true;
    api
      .agentSettings()
      .then((snapshot) => live && setModels(snapshot.config.models.map((m) => m.id)))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

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
      const { profile } = await api.createAgentProfile({
        alias: name,
        catalogue: role.id,
        // The role's job is the default; only an edited job is sent.
        job: job !== role.instructions ? job : undefined,
        model: model !== INHERIT ? model : undefined,
      });
      onHired(profile, role);
    } catch (error) {
      setErr(error instanceof ApiError || error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  const why = hireBlock !== null ? t("Only the operator can hire.") : t("Nothing runs until a runtime connects.");
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
          aria-label={t("Close")}
          onClick={onClose}
        >
          ✕
        </Button>
      </div>
      <div className="rc-sheet-b">
        <HireGate reason={hireBlock} />
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

        <label className="rc-fld">
          <span className="lab">{t("Model")}</span>
          <select className="field" value={model} onChange={(event) => setModel(event.target.value)}>
            <option value={INHERIT}>{t("Inherit from the runtime")}</option>
            {models.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </select>
          <span className="hint">{t("A model you pick is used as chosen or refused, never swapped.")}</span>
        </label>
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
            <dt>{t("catalogue")}</dt>
            <dd>
              {role.id} {t("(recorded on the profile)")}
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
          title={hireBlock ?? undefined}
          onClick={() => void hire()}
        >
          {saving ? t("Adding…") : t("Add {name}").replace("{name}", () => alias.trim())}
        </Button>
      </div>
    </dialog>
  );
}
