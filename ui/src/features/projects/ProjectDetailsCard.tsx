import { useEffect, useId, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { useLocale } from "../../lib/locale";
import { privateSessionGeneration } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import type { Project, ProjectPreset, UpdateProjectInput } from "../../lib/types";
import { useQuery } from "../../lib/useResource";
import {
  PROJECT_PRESETS,
  normalizeProjectPreset,
  projectDisplayName,
  updateProjectErrorMessage,
} from "./projectCreate";

export function ProjectDetailsCard({
  projectKey,
  projectInfo,
  readOnlyReason,
  onProjectResolved,
  onToast,
}: {
  projectKey: string;
  projectInfo?: Project | null;
  readOnlyReason: string | null;
  onProjectResolved?: (project: Project | null) => void;
  onToast: (text: string) => void;
}) {
  const { t } = useLocale();
  const projectsState = useQuery(resources.projects);
  const resolvedProject = useMemo(
    () =>
      projectsState.data?.find((item) => item.key === projectKey) ??
      projectInfo ??
      null,
    [projectsState.data, projectInfo, projectKey],
  );

  const baseName = projectDisplayName(resolvedProject ?? { key: projectKey });
  const baseGoal = resolvedProject?.goal?.trim() ?? "";
  const basePreset: ProjectPreset | "" = normalizeProjectPreset(resolvedProject?.preset);
  const baselineSignature = `${projectKey}\n${baseName}\n${baseGoal}\n${basePreset}`;

  const [name, setName] = useState(baseName);
  const [goal, setGoal] = useState(baseGoal);
  const [preset, setPreset] = useState<ProjectPreset | "">(basePreset);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const nameId = useId();
  const goalId = useId();
  const presetLabelId = useId();

  const lastSyncedRef = useRef({
    key: projectKey,
    signature: baselineSignature,
    userEdited: false,
  });
  const activeRef = useRef({ key: projectKey, epoch: 0, mounted: true });
  if (activeRef.current.key !== projectKey) {
    activeRef.current = {
      key: projectKey,
      epoch: activeRef.current.epoch + 1,
      mounted: true,
    };
  }
  useEffect(() => {
    activeRef.current.mounted = true;
    return () => {
      activeRef.current.mounted = false;
      activeRef.current.epoch += 1;
    };
  }, []);

  useEffect(() => {
    onProjectResolved?.(resolvedProject);
  }, [resolvedProject, onProjectResolved]);

  useEffect(() => {
    if (lastSyncedRef.current.key !== projectKey) {
      setName(baseName);
      setGoal(baseGoal);
      setPreset(basePreset);
      setBusy(false);
      setError(null);
      lastSyncedRef.current = {
        key: projectKey,
        signature: baselineSignature,
        userEdited: false,
      };
      return;
    }
    if (
      lastSyncedRef.current.signature !== baselineSignature &&
      !lastSyncedRef.current.userEdited
    ) {
      setName(baseName);
      setGoal(baseGoal);
      setPreset(basePreset);
      lastSyncedRef.current.signature = baselineSignature;
    }
  }, [projectKey, baselineSignature, baseName, baseGoal, basePreset]);

  const locked = readOnlyReason !== null;
  useEffect(() => {
    if (locked) setError(null);
  }, [locked]);
  const trimmedName = name.trim();
  const trimmedGoal = goal.trim();
  const dirty =
    trimmedName !== baseName.trim() ||
    trimmedGoal !== baseGoal ||
    preset !== basePreset;

  function markEdited() {
    lastSyncedRef.current.userEdited = true;
    if (error !== null) setError(null);
  }

  function resetDraft() {
    setName(baseName);
    setGoal(baseGoal);
    setPreset(basePreset);
    setError(null);
    lastSyncedRef.current = {
      key: projectKey,
      signature: baselineSignature,
      userEdited: false,
    };
  }

  async function saveDetails() {
    if (locked || busy) return;
    if (trimmedName.length === 0) {
      setError(t("Project name is required."));
      return;
    }
    if (!dirty) return;

    const payload: UpdateProjectInput = {};
    if (trimmedName !== baseName.trim()) payload.name = trimmedName;
    if (trimmedGoal !== baseGoal) payload.goal = trimmedGoal;
    if (preset !== basePreset) payload.preset = preset;

    const epoch = activeRef.current.epoch;
    const generation = privateSessionGeneration();
    const isCurrent = () =>
      activeRef.current.mounted &&
      activeRef.current.epoch === epoch &&
      privateSessionGeneration() === generation;

    setBusy(true);
    setError(null);
    try {
      const resp = await api.updateProject(projectKey, payload);
      if (!isCurrent()) return;
      const saved = resp.project;
      const nextName = projectDisplayName(saved);
      const nextGoal = saved.goal?.trim() ?? "";
      const nextPreset: ProjectPreset | "" = normalizeProjectPreset(saved.preset);
      setName(nextName);
      setGoal(nextGoal);
      setPreset(nextPreset);
      lastSyncedRef.current = {
        key: projectKey,
        signature: `${projectKey}\n${nextName}\n${nextGoal}\n${nextPreset}`,
        userEdited: false,
      };
      resources.projects.write((prev) => {
        if (!prev) return [saved];
        const exists = prev.some((item) => item.key === saved.key);
        return exists
          ? prev.map((item) => (item.key === saved.key ? { ...item, ...saved } : item))
          : [...prev, saved];
      });
      void resources.projects.refresh();
      onProjectResolved?.(saved);
      onToast(t("Project details saved."));
    } catch (err) {
      if (!isCurrent()) return;
      setError(updateProjectErrorMessage(err, t));
    } finally {
      if (isCurrent()) setBusy(false);
    }
  }

  return (
    <section
      className="project-summary-card project-details-card mb-4 max-w-2xl"
      aria-label={t("Project details")}
    >
      <div className="project-details-head">
        <div>
          <div className="slabel">{t("Project details")}</div>
          <p className="mt-1 text-label text-ink-400">
            {t("Display name, summary goal, and domain preset.")}
          </p>
        </div>
        <span className="num text-micro text-ink-500">{projectKey}</span>
      </div>

      <form
        className="project-create-form mt-4"
        onSubmit={(event) => {
          event.preventDefault();
          void saveDetails();
        }}
        noValidate
      >
        <div className="project-create-field">
          <label className="project-create-label" htmlFor={nameId}>
            {t("Project name")}
          </label>
          <input
            id={nameId}
            className="project-create-input"
            type="text"
            value={name}
            disabled={locked || busy}
            onChange={(event) => {
              markEdited();
              setName(event.target.value);
            }}
            placeholder={projectKey}
            maxLength={120}
            required
          />
        </div>

        <div className="project-create-field">
          <label className="project-create-label" htmlFor={goalId}>
            {t("1-line goal")}{" "}
            <span className="project-create-optional">({t("optional")})</span>
          </label>
          <input
            id={goalId}
            className="project-create-input"
            type="text"
            value={goal}
            disabled={locked || busy}
            onChange={(event) => {
              markEdited();
              setGoal(event.target.value);
            }}
            placeholder={t("What is this project delivering?")}
            maxLength={160}
          />
        </div>

        <div className="project-create-field">
          <span className="project-create-label" id={presetLabelId}>
            {t("Domain preset")}{" "}
            <span className="project-create-optional">({t("optional")})</span>
          </span>
          <div
            className="project-create-presets"
            role="group"
            aria-labelledby={presetLabelId}
          >
            {PROJECT_PRESETS.map((item) => {
              const selected = preset === item.value;
              return (
                <button
                  key={item.value}
                  type="button"
                  className={`project-create-preset${selected ? " is-selected" : ""}`}
                  aria-pressed={selected}
                  disabled={locked || busy}
                  onClick={() => {
                    markEdited();
                    setPreset(selected ? "" : item.value);
                  }}
                >
                  {t(item.label)}
                </button>
              );
            })}
          </div>
          {!locked && (
            <p className="project-create-hint">
              {t("Click the active preset again to clear it.")}
            </p>
          )}
        </div>

        {readOnlyReason && (
          <p className="text-label text-ink-400">{readOnlyReason}</p>
        )}

        {error && (
          <p className="project-create-error" role="alert">
            {error}
          </p>
        )}

        {!locked && (
          <div className="project-details-foot">
            <button
              type="submit"
              className="btn btn-primary"
              disabled={busy || !dirty || trimmedName.length === 0}
            >
              {busy ? t("Saving…") : t("Save details")}
            </button>
            {dirty && !busy && (
              <button
                type="button"
                className="btn btn-ghost"
                onClick={resetDraft}
              >
                {t("Reset")}
              </button>
            )}
          </div>
        )}
      </form>
    </section>
  );
}
