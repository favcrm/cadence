import { useId, useRef, useState, type FormEvent } from "react";
import { api, type WriteResp } from "../../lib/api";
import type { Project } from "../../lib/types";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { useLocale } from "../../lib/locale";

/**
 * What the create form files: a plain task, or a report — a
 * question, feedback, idea or bug (CAD-140). Reports file with the
 * `intake` + kind system tags, exactly like `cadence report`, so the
 * idea pipeline and the intake triage see them; an idea filed here
 * triggers research and a plan draft like one filed from the CLI.
 */
type NewIssueKind = "task" | "question" | "feedback" | "idea" | "bug";

const KIND_OPTIONS: { value: NewIssueKind; label: string }[] = [
  { value: "task", label: "Task" },
  { value: "question", label: "Question" },
  { value: "feedback", label: "Feedback" },
  { value: "idea", label: "Idea" },
  { value: "bug", label: "Bug" },
];

/** One create form for both list and board views. The draft survives a failed write. */
export default function NewIssueForm({
  projects,
  project,
  onCreated,
  onError,
  onCancel,
  readOnly,
  writeReason,
}: {
  projects: Project[];
  project: string;
  onCreated: (resp: WriteResp, verb: string) => void;
  onError: (e: unknown, verb: string) => void;
  onCancel: () => void;
  readOnly: boolean;
  writeReason: string | null;
}) {
  const { t } = useLocale();
  const [title, setTitle] = useState("");
  const [kind, setKind] = useState<NewIssueKind>("task");
  const [details, setDetails] = useState("");
  const [priority, setPriority] = useState("P2");
  const [selProject, setSelProject] = useState(() => project === "all" ? projects[0]?.key ?? "" : project);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef(false);
  const fieldId = useId();
  const projectId = useId();
  const priorityId = useId();
  const kindId = useId();
  const detailsId = useId();
  const projectKey = project === "all" ? selProject : project;
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const t = title.trim();
    if (!t || !projectKey || readOnly || pending.current) return;
    pending.current = true;
    setError(null);
    setBusy(true);
    const detailsText = details.trim();
    // CAD-140: reports file through the canonical intake endpoint
    // (`POST /api/reports` → `report::file`) — never a bare create.
    // Ideas land on this board; every other kind routes to the
    // `cadence` project server-side, like `cadence report`.
    const req =
      kind === "task"
        ? api.create({
            project: projectKey,
            title: t,
            priority,
            ...(detailsText ? { body: `${t}\n\n${detailsText}` } : {}),
          })
        : api.report({
            kind,
            project: projectKey,
            title: t,
            priority,
            ...(detailsText ? { body: detailsText } : {}),
          });
    req
      .then((resp) => {
        onCreated(resp, `${resp.card.id} created`);
        onCancel();
      })
      .catch((e) => {
        setError(e instanceof Error ? e.message : String(e));
        onError(e, "create");
      })
      .finally(() => {
        pending.current = false;
        setBusy(false);
      });
  };
  return (
    <form className="card board-new-issue mb-4 p-4 border-accent/40 reveal" onSubmit={submit} onKeyDown={(event) => {
      // The shared Select uses Escape to close its own popup. Portal key
      // events still bubble through this form, so only Escape in Title
      // cancels the draft.
      if (event.key === "Escape" && !busy && event.target instanceof HTMLInputElement && event.target.name === "title") {
        event.preventDefault();
        onCancel();
      }
    }}>
      <div className="flex items-start gap-3 mb-3">
        <div className="min-w-0">
          <h2 className="text-cardtitle font-semibold text-ink-100">{t("New issue")}</h2>
          <p className="text-label text-ink-500 mt-0.5">{t("Start work in")} {projectKey || t("a project")}.</p>
        </div>
        <Button className="ml-auto" variant="ghost" size="sm" onClick={onCancel} disabled={busy}>{t("Cancel")}</Button>
      </div>
      <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(9rem,14rem)_7rem] items-end">
        <div className="min-w-0">
          <label className="slabel block mb-1" htmlFor={fieldId}>{t("Title")}</label>
          <input id={fieldId} name="title" autoFocus required maxLength={200} value={title} onChange={(event) => setTitle(event.target.value)} disabled={busy || readOnly} className="field w-full" placeholder={t("What needs to happen?")} />
        </div>
        {project === "all" ? (
          <div className="min-w-0">
            <label className="slabel block mb-1" htmlFor={projectId}>{t("Project")}</label>
            <Select id={projectId} full value={projectKey} onChange={setSelProject} disabled={busy || readOnly || projects.length === 0} options={projects.map((p) => ({ value: p.key, label: p.key }))} />
          </div>
        ) : <div className="min-w-0"><span className="slabel block mb-1">{t("Project")}</span><span className="block text-label text-ink-300 py-2">{projectKey}</span></div>}
        <div className="min-w-0">
          <label className="slabel block mb-1" htmlFor={priorityId}>{t("Priority")}</label>
          <Select id={priorityId} full value={priority} onChange={setPriority} disabled={busy || readOnly} options={["P0", "P1", "P2", "P3"].map((p) => ({ value: p, label: p }))} />
        </div>
      </div>
      <div className="grid gap-3 sm:grid-cols-[minmax(9rem,14rem)_minmax(0,1fr)] items-start mt-3">
        <div className="min-w-0">
          <label className="slabel block mb-1" htmlFor={kindId}>{t("Kind")}</label>
          <Select id={kindId} full value={kind} onChange={(v) => { const k = v as NewIssueKind; setKind(k); setPriority(k === "task" || k === "bug" ? "P2" : "P3"); }} disabled={busy || readOnly} options={KIND_OPTIONS.map((option) => ({ ...option, label: t(option.label) }))} />
          {kind === "idea" && (
            <p className="text-micro text-ink-500 mt-1">{t("Files into")} {projectKey || t("this project")} {t("and triggers research + a plan draft.")}</p>
          )}
          {kind !== "task" && kind !== "idea" && (
            <p className="text-micro text-ink-500 mt-1">{t("Files into the cadence project for triage.")}</p>
          )}
        </div>
        <div className="min-w-0">
          <label className="slabel block mb-1" htmlFor={detailsId}>{t("Details")} <span className="text-ink-500">({t("optional")})</span></label>
          <textarea id={detailsId} rows={3} maxLength={32768} value={details} onChange={(event) => setDetails(event.target.value)} disabled={busy || readOnly} className="field w-full !h-auto py-2 text-secondary" placeholder={t(kind === "task" ? "Acceptance, context, links…" : "What happened, what you expected, what you tried…")} />
        </div>
      </div>
      {error && <p className="text-label text-fail mt-3" role="alert">{t("Could not create issue:")} {error}</p>}
      {readOnly && <p className="text-label text-ink-400 mt-3" role="status">{t("Draft saved.")} {writeReason ?? t("Writes are unavailable.")}</p>}
      <div className="flex justify-end mt-3">
        <Button variant="primary" type="submit" loading={busy} disabled={readOnly || !title.trim() || !projectKey}>{kind === "task" ? t("Create issue") : `${t("File")} ${t(kind)}`}</Button>
      </div>
    </form>
  );
}
