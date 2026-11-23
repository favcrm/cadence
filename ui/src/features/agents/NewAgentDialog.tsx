import { useContext, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import Button from "../../ui/Button";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { HireBlock } from "../auth/WriteGate";
import { useTranslate } from "../../lib/locale";
import { buildBody } from "./agentSetup";

export default function NewAgentDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const t = useTranslate();
  const hireBlock = useContext(HireBlock);
  const [alias, setAlias] = useState("");
  const [provider, setProvider] = useState("");
  const [endpointKind, setEndpointKind] = useState("");
  const [role, setRole] = useState("");
  const [teamRole, setTeamRole] = useState("");
  const [modelPolicy, setModelPolicy] = useState("");
  const [sandbox, setSandbox] = useState("");
  const [instructions, setInstructions] = useState("");
  const [cwd, setCwd] = useState("");
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const save = async () => {
    if (hireBlock !== null || saving) return;
    // The runtime follows the install; Start blank is the local form.
    const built = buildBody("local", {
      alias,
      provider,
      endpointKind,
      role,
      teamRole,
      modelPolicy,
      sandbox,
      instructions,
      cwd,
    });
    if ("error" in built) {
      setErr(built.error);
      return;
    }
    setSaving(true);
    setErr(null);
    try {
      await api.registerAgent(built.body);
      onCreated();
      onClose();
    } catch (error) {
      setErr(error instanceof ApiError || error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  return (
    <DrawerShell
      kind="new-agent"
      scrim="board"
      modal
      trapFocus
      size="md"
      preventClose={saving}
      label={t("New agent")}
      title={t("New agent")}
      onClose={onClose}
      foot={
        <>
          <div className="s-drawer-actions crm-drawer-actions">
            <Button variant="ghost" size="sm" onClick={onClose} disabled={saving}>
              {t("Cancel")}
            </Button>
            <button
              type="submit"
              form="new-agent-form"
              className="btn btn-sm btn-primary"
              disabled={hireBlock !== null || saving}
              aria-busy={saving || undefined}
              title={hireBlock ?? undefined}
            >
              {saving && <span className="btn-spin" aria-hidden="true" />}
              <span className="btn-label">{saving ? t("Saving…") : t("Create agent")}</span>
            </button>
          </div>
          {hireBlock && <span className="s-drawer-note crm-drawer-note">{hireBlock}</span>}
        </>
      }
    >
      <form
        id="new-agent-form"
        className="agents-drawer-body flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label className="field-control">
          <span className="slabel">{t("Alias")}</span>
          <input className="field" value={alias} onChange={(event) => setAlias(event.target.value)} />
        </label>

        <label className="field-control">
          <span className="slabel">{t("Provider")}</span>
          <input className="field" value={provider} onChange={(event) => setProvider(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Endpoint kind")}</span>
          <input className="field" value={endpointKind} onChange={(event) => setEndpointKind(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Working directory")}</span>
          <input className="field" value={cwd} onChange={(event) => setCwd(event.target.value)} />
        </label>

        <label className="field-control">
          <span className="slabel">{t("Role")}</span>
          <input className="field" value={role} placeholder="worker" onChange={(event) => setRole(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Team role")}</span>
          <input className="field" value={teamRole} onChange={(event) => setTeamRole(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Model policy")}</span>
          <input className="field" value={modelPolicy} onChange={(event) => setModelPolicy(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Sandbox")}</span>
          <input className="field" value={sandbox} placeholder="read-only" onChange={(event) => setSandbox(event.target.value)} />
        </label>
        <label className="field-control">
          <span className="slabel">{t("Instructions")}</span>
          <textarea className="field" value={instructions} onChange={(event) => setInstructions(event.target.value)} />
        </label>

        {err && (
          <p role="alert" className="text-label text-fail">
            {err}
          </p>
        )}
      </form>
    </DrawerShell>
  );
}
