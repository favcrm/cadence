import { useContext, useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import Button from "../../ui/Button";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { HireBlock } from "../auth/WriteGate";
import type { HiredProfile, AgentRole, AgentRolesPayload } from "../../lib/types";
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
  onHired: (profile: HiredProfile, role: AgentRole) => void;
}) {
  const t = useTranslate();
  const hireBlock = useContext(HireBlock);
  const appsState = useQuery(resources.apps);
  const installedApps = useMemo(
    () => (appsState.data ? new Set(appsState.data.map((app) => app.name)) : null),
    [appsState.data],
  );
  const recommendedApps = role.recommended_apps ?? [];
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
    <DrawerShell
      kind="hire"
      scrim="board"
      modal
      trapFocus
      size="md"
      preventClose={saving}
      label={role.title}
      avatar={<AgentAvatar slug="custom" look={role.look} size={44} active />}
      title={<span id="hire-title">{role.title}</span>}
      subtitle={`${t(categoryLabel(payload, role.category))} · ${t("built-in role")}`}
      onClose={onClose}
      foot={
        <>
          <div className="s-drawer-actions crm-drawer-actions">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={saving}>
              {t("Cancel")}
            </Button>
            <Button
              variant="primary"
              size="sm"
              disabled={blocked || saving}
              loading={saving}
              title={hireBlock ?? undefined}
              onClick={() => void hire()}
            >
              {saving ? t("Adding…") : t("Add {name}").replace("{name}", () => alias.trim())}
            </Button>
          </div>
          {err ? (
            <span className="s-drawer-note crm-drawer-note text-fail" role="alert">
              {err}
            </span>
          ) : (
            <span className="s-drawer-note crm-drawer-note">{why}</span>
          )}
        </>
      }
    >
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
            {role.context_seed ? <Chip text="Starter playbook seeded at hire" /> : null}
          </div>
        </div>
        {recommendedApps.length > 0 && (
          <div className="rc-blk">
            <h4>{t("Recommended apps")}</h4>
            <div className="rc-kst-row">
              {recommendedApps.map((app) => {
                const accessLabel = app.access === "draft" ? t("Read & draft") : t("Read-only");
                const installLabel =
                  installedApps === null
                    ? null
                    : installedApps.has(app.app_key)
                      ? t("Installed")
                      : t("Install later");
                const parts = [app.app_key, accessLabel, installLabel].filter(Boolean);
                return (
                  <span key={app.app_key} className="rc-kst">
                    {parts.join(" · ")}
                  </span>
                );
              })}
            </div>
            <p className="rc-note">
              {t(
                "Bound apps restrict which workspace apps this specialist can call. Install missing apps anytime in Apps.",
              )}
            </p>
          </div>
        )}
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
            {role.skills.length > 0
              ? t("Role skills and connected app skills load on demand in each session.")
              : t("Uses its role instructions and connected app skills on demand.")}
          </p>
        </div>
        <div className="rc-blk">
          <h4>{t("Tools")}</h4>
          <div className="rc-kst-row">
            {role.tools.map((tool) => (
              <Chip key={tool} text={tool} />
            ))}
            <Chip text="Owner approval required before external sends" />
          </div>
          {role.disabled_tools && role.disabled_tools.length > 0 ? (
            <p className="rc-note">
              {t("Disabled tool groups: {groups}").replace("{groups}", () =>
                role.disabled_tools!.join(", "),
              )}
            </p>
          ) : null}
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
      </div>
    </DrawerShell>
  );
}
