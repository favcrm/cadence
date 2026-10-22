import { useContext, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { IconClose } from "../../ui/icons";
import Button from "../../ui/Button";
import { WriteGate } from "../auth/WriteGate";
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
  const writeBlock = useContext(WriteGate);
  const dialogRef = useRef<HTMLDialogElement>(null);
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

  const save = async () => {
    if (writeBlock !== null || saving) return;
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
    <dialog
      ref={dialogRef}
      className="agents-drawer bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby="new-agent-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      <form
        className="flex flex-col gap-4 p-5"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <div className="flex items-center justify-between gap-3">
          <h2 id="new-agent-title" className="text-section font-semibold text-ink-100">
            {t("New agent")}
          </h2>
          <Button
            variant="ghost"
            size="sm"
            icon={<IconClose />}
            aria-label={t("Close")}
            onClick={onClose}
          />
        </div>

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
        {writeBlock && <p className="text-label text-ink-500">{writeBlock}</p>}

        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            {t("Cancel")}
          </Button>
          <Button type="submit" variant="primary" size="sm" disabled={writeBlock !== null || saving} title={writeBlock ?? undefined}>
            {saving ? t("Saving…") : t("Create agent")}
          </Button>
        </div>
      </form>
    </dialog>
  );
}
