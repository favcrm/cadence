import { useEffect, useId, useRef, useState } from "react";
import type React from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/localeText";
import { PROJECT_KEY } from "../../lib/projectKey";
import type { CreateProjectInput, CreateProjectResponse, ProjectPreset } from "../../lib/types";
import { IconChevron } from "../../ui/icons";
import {
  PROJECT_PREFIX,
  PROJECT_PRESETS,
  createProjectErrorMessage,
  deriveProjectPrefix,
  slugifyProjectKey,
} from "./projectCreate";

export default function NewProjectModal({
  open,
  hosted,
  visibilitySupported = true,
  returnFocusTo,
  onClose,
  onCreated,
}: {
  open: boolean;
  hosted: boolean;
  visibilitySupported?: boolean;
  returnFocusTo?: HTMLElement | null;
  onClose: () => void;
  onCreated: (created: CreateProjectResponse) => void;
}) {
  const t = useTranslate();
  const titleId = useId();
  const nameId = useId();
  const goalId = useId();
  const advancedId = useId();
  const keyId = useId();
  const prefixId = useId();
  const repoId = useId();

  const dialogRef = useRef<HTMLElement | null>(null);
  const nameInputRef = useRef<HTMLInputElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);

  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [preset, setPreset] = useState<ProjectPreset | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [key, setKey] = useState("");
  const [keyEdited, setKeyEdited] = useState(false);
  const [prefix, setPrefix] = useState("");
  const [prefixEdited, setPrefixEdited] = useState(false);
  const [visibility, setVisibility] = useState<"workspace-visible" | "private">(
    hosted ? "private" : "workspace-visible",
  );
  const [repo, setRepo] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    const active = document.activeElement;
    openerRef.current = active instanceof HTMLElement ? active : null;
    setName("");
    setGoal("");
    setPreset(null);
    setAdvancedOpen(false);
    setKey("");
    setKeyEdited(false);
    setPrefix("");
    setPrefixEdited(false);
    setVisibility(hosted ? "private" : "workspace-visible");
    setRepo("");
    setBusy(false);
    setError(null);
    requestAnimationFrame(() => {
      nameInputRef.current?.focus();
    });
    return () => {
      const target = returnFocusTo ?? openerRef.current;
      if (target?.isConnected) target.focus();
    };
  }, [open, hosted, returnFocusTo]);

  if (!open) return null;

  const trimmedName = name.trim();
  const derivedKey = slugifyProjectKey(trimmedName);
  const effectiveKey = (keyEdited ? key : derivedKey).trim();
  const derivedPrefix = deriveProjectPrefix(effectiveKey);
  const effectivePrefix = (prefixEdited ? prefix : derivedPrefix).trim().toUpperCase();

  const focusable = (): HTMLElement[] => {
    const root = dialogRef.current;
    if (!root) return [];
    return Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.getAttribute("aria-hidden") !== "true");
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (!busy) onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      dialogRef.current?.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || active === dialogRef.current) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || active === dialogRef.current) {
      event.preventDefault();
      first.focus();
    }
  };

  const handleNameChange = (nextName: string) => {
    setName(nextName);
    setError(null);
    const nextSlug = slugifyProjectKey(nextName);
    if (!keyEdited) {
      setKey(nextSlug);
      if (!prefixEdited) {
        setPrefix(deriveProjectPrefix(nextSlug));
      }
    }
  };

  const handleKeyChange = (nextKey: string) => {
    setKeyEdited(true);
    setKey(nextKey);
    setError(null);
    if (!prefixEdited) {
      setPrefix(deriveProjectPrefix(nextKey.trim()));
    }
  };

  const handlePrefixChange = (nextPrefix: string) => {
    setPrefixEdited(true);
    setPrefix(nextPrefix.toUpperCase());
    setError(null);
  };

  const handleSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy) return;
    if (!trimmedName) {
      setError(t("Project name is required."));
      nameInputRef.current?.focus();
      return;
    }
    if (!PROJECT_KEY.test(effectiveKey)) {
      setAdvancedOpen(true);
      setError(t("Enter a valid project key (1-32 lowercase letters, digits, or hyphens)."));
      return;
    }
    if (effectivePrefix !== "" && !PROJECT_PREFIX.test(effectivePrefix)) {
      setAdvancedOpen(true);
      setError(t("ID prefix must be 1-8 uppercase letters or digits starting with a letter."));
      return;
    }

    const trimmedGoal = goal.trim();
    const trimmedRepo = repo.trim();
    const payload: CreateProjectInput = {
      name: trimmedName,
      key: effectiveKey,
      ...(effectivePrefix ? { prefix: effectivePrefix } : {}),
      ...(trimmedGoal ? { goal: trimmedGoal } : {}),
      ...(preset ? { preset } : {}),
      ...(visibilitySupported ? { visibility } : {}),
      ...(!hosted && trimmedRepo ? { repo: trimmedRepo } : {}),
    };

    setBusy(true);
    setError(null);
    try {
      const created = await api.createProject(payload);
      onCreated(created);
    } catch (err) {
      if (err instanceof ApiError && (err.code === "project_exists" || err.code === "prefix_conflict")) {
        setAdvancedOpen(true);
      }
      setError(createProjectErrorMessage(err, t));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="s-confirm-wrap" role="presentation">
      <div
        className="s-confirm-scrim"
        onClick={() => {
          if (!busy) onClose();
        }}
      />
      <section
        ref={dialogRef}
        tabIndex={-1}
        className="project-create-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={handleKeyDown}
      >
        <header className="project-create-head">
          <div>
            <div className="slabel">{t("Your workspace")}</div>
            <h2 id={titleId} className="project-create-title">
              {t("New project")}
            </h2>
          </div>
          <button
            type="button"
            className="btn btn-sm btn-ghost"
            onClick={() => {
              if (!busy) onClose();
            }}
            disabled={busy}
            aria-label={t("Close")}
          >
            ✕
          </button>
        </header>

        <form className="project-create-form" onSubmit={(e) => void handleSubmit(e)} noValidate>
          <div className="project-create-field">
            <label htmlFor={nameId} className="project-create-label">
              {t("Project name")}
            </label>
            <input
              id={nameId}
              ref={nameInputRef}
              type="text"
              className="project-create-input"
              value={name}
              onChange={(e) => handleNameChange(e.target.value)}
              placeholder={t("e.g. Customer Onboarding")}
              maxLength={120}
              autoComplete="off"
              required
              disabled={busy}
            />
          </div>

          <div className="project-create-field">
            <label htmlFor={goalId} className="project-create-label">
              {t("1-line goal")} <span className="project-create-optional">({t("optional")})</span>
            </label>
            <input
              id={goalId}
              type="text"
              className="project-create-input"
              value={goal}
              onChange={(e) => {
                setGoal(e.target.value);
                setError(null);
              }}
              placeholder={t("What this project aims to achieve")}
              maxLength={240}
              autoComplete="off"
              disabled={busy}
            />
            {!hosted && goal.trim().length > 0 && repo.trim().length === 0 && (
              <p className="project-create-hint">
                {t("On native boards, setting a goal also requires a repository path in Advanced.")}
              </p>
            )}
          </div>

          <div className="project-create-field">
            <div className="project-create-label">
              {t("Domain preset")} <span className="project-create-optional">({t("optional")})</span>
            </div>
            <div className="project-create-presets" role="group" aria-label={t("Domain preset")}>
              {PROJECT_PRESETS.map((opt) => {
                const selected = preset === opt.value;
                return (
                  <button
                    key={opt.value}
                    type="button"
                    className={`project-create-preset${selected ? " is-selected" : ""}`}
                    aria-pressed={selected}
                    onClick={() => setPreset(selected ? null : opt.value)}
                    disabled={busy}
                    title={t(opt.hint)}
                  >
                    {t(opt.label)}
                  </button>
                );
              })}
            </div>
          </div>

          <div className="project-create-advanced">
            <button
              type="button"
              className="project-create-advanced-toggle"
              aria-expanded={advancedOpen}
              aria-controls={advancedId}
              onClick={() => setAdvancedOpen((prev) => !prev)}
            >
              <span className="flex items-center gap-2 min-w-0">
                <IconChevron className={`shrink-0 transition-transform ${advancedOpen ? "rotate-180" : ""}`} />
                <span>{t("Advanced")}</span>
              </span>
              {!advancedOpen && (effectiveKey || effectivePrefix) && (
                <span className="num project-create-advanced-preview">
                  {[effectiveKey, effectivePrefix].filter(Boolean).join(" · ")}
                </span>
              )}
            </button>

            {advancedOpen && (
              <div id={advancedId} className="project-create-advanced-body">
                <div className="project-create-row">
                  <div className="project-create-field">
                    <label htmlFor={keyId} className="project-create-label">
                      {t("Key")}
                    </label>
                    <input
                      id={keyId}
                      type="text"
                      className="project-create-input num"
                      value={keyEdited ? key : derivedKey}
                      onChange={(e) => handleKeyChange(e.target.value)}
                      placeholder="customer-onboarding"
                      maxLength={32}
                      autoComplete="off"
                      disabled={busy}
                    />
                  </div>
                  <div className="project-create-field">
                    <label htmlFor={prefixId} className="project-create-label">
                      {t("ID prefix")}
                    </label>
                    <input
                      id={prefixId}
                      type="text"
                      className="project-create-input num"
                      value={prefixEdited ? prefix : derivedPrefix}
                      onChange={(e) => handlePrefixChange(e.target.value)}
                      placeholder="CUS"
                      maxLength={8}
                      autoComplete="off"
                      disabled={busy}
                    />
                  </div>
                </div>

                {visibilitySupported && (
                  <div className="project-create-field">
                    <div className="project-create-label">{t("Visibility")}</div>
                    <div className="project-create-visibility" role="group" aria-label={t("Visibility")}>
                      {(
                        [
                          { value: "workspace-visible", label: "Workspace", desc: "Visible to workspace members" },
                          { value: "private", label: "Private", desc: "Restricted to granted members" },
                        ] as const
                      ).map((opt) => {
                        const selected = visibility === opt.value;
                        return (
                          <button
                            key={opt.value}
                            type="button"
                            className={`project-create-vis-opt${selected ? " is-selected" : ""}`}
                            aria-pressed={selected}
                            onClick={() => {
                              setVisibility(opt.value);
                              setError(null);
                            }}
                            disabled={busy}
                          >
                            <span className="project-create-vis-title">{t(opt.label)}</span>
                            <span className="project-create-vis-desc">{t(opt.desc)}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                )}

                {!hosted && (
                  <div className="project-create-field">
                    <label htmlFor={repoId} className="project-create-label">
                      {t("Repository")} <span className="project-create-optional">({t("optional")})</span>
                    </label>
                    <input
                      id={repoId}
                      type="text"
                      className="project-create-input num"
                      value={repo}
                      onChange={(e) => {
                        setRepo(e.target.value);
                        setError(null);
                      }}
                      placeholder="/path/to/git/checkout"
                      autoComplete="off"
                      disabled={busy}
                    />
                  </div>
                )}
              </div>
            )}
          </div>

          {error && (
            <div className="project-create-error" role="alert">
              {error}
            </div>
          )}

          <footer className="project-create-foot">
            <button
              type="button"
              className="btn btn-ghost"
              onClick={onClose}
              disabled={busy}
            >
              {t("Cancel")}
            </button>
            <button
              type="submit"
              className="btn btn-primary"
              disabled={busy || trimmedName.length === 0}
            >
              {busy ? t("Creating…") : t("Create project")}
            </button>
          </footer>
        </form>
      </section>
    </div>
  );
}
