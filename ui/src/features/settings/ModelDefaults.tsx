import { useCallback, useContext, useEffect, useMemo, useState } from "react";
import { READ_ONLY_REASON } from "../auth/gate";
import { WriteGate } from "../auth/WriteGate";
import { api, ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { IconRefresh } from "../../ui/icons";
import "./modelDefaults.css";
import { canonicalJson } from "./modelDefaultsCompare";
import { modelIdProblem } from "./modelId";
import type {
  ModelDefaultsConfig,
  ModelDefaultsSnapshot,
  ModelProviderInfo,
  ModelRoleInfo,
  ProviderModelDefaults,
} from "../../lib/types";

type BaselineChoice = "provider_default" | "model";
type RoleChoice = "inherit" | "provider_default" | "model";

interface RoleDraft {
  choice: RoleChoice;
  model: string;
}

interface ProviderDraft {
  /** False omits the provider key, so registrations inherit an empty baseline. */
  present: boolean;
  baseline: BaselineChoice;
  baselineModel: string;
  roles: Record<string, RoleDraft>;
}

function roleDraft(choice: RoleChoice = "inherit", model = ""): RoleDraft {
  return { choice, model };
}

function draftsFrom(
  snapshot: ModelDefaultsSnapshot,
): Record<string, ProviderDraft> {
  const out: Record<string, ProviderDraft> = {};
  for (const provider of snapshot.providers) {
    const stored = snapshot.config.providers[provider.id];
    const roles: Record<string, RoleDraft> = {};
    for (const role of snapshot.roles) {
      const selector = stored?.roles?.[role.id];
      if (!selector) roles[role.id] = roleDraft();
      else if (selector.mode === "provider_default") {
        roles[role.id] = roleDraft("provider_default");
      } else {
        roles[role.id] = roleDraft("model", selector.model ?? "");
      }
    }
    out[provider.id] = {
      present: Boolean(stored),
      baseline: stored?.default.mode === "model" ? "model" : "provider_default",
      baselineModel:
        stored?.default.mode === "model" ? (stored.default.model ?? "") : "",
      roles,
    };
  }
  return out;
}

function selector(choice: BaselineChoice | RoleChoice, model: string) {
  if (choice === "model")
    return { mode: "model" as const, model: model.trim() };
  return { mode: "provider_default" as const };
}

function configFrom(
  providers: ModelProviderInfo[],
  roles: ModelRoleInfo[],
  drafts: Record<string, ProviderDraft>,
): ModelDefaultsConfig {
  const stored: Record<string, ProviderModelDefaults> = {};
  for (const provider of providers) {
    const draft = drafts[provider.id];
    if (!provider.eligible || !draft?.present) continue;
    const roleMap: Record<string, ReturnType<typeof selector>> = {};
    for (const role of roles) {
      const row = draft.roles[role.id] ?? roleDraft();
      if (row.choice === "inherit") continue;
      roleMap[role.id] = selector(row.choice, row.model);
    }
    stored[provider.id] = {
      default: selector(draft.baseline, draft.baselineModel),
      roles: roleMap,
    };
  }
  return { schema: 1, providers: stored };
}

function statusCopy(status: number, code?: string): string {
  if (status === 503 || code === "daemon_unavailable") {
    return "The daemon is not reachable. Settings stay unchanged until it answers.";
  }
  if (status === 501 || code === "unsupported_daemon") {
    return "This daemon does not support model defaults. Restart is not attempted from the board.";
  }
  return "";
}

export default function ModelDefaults() {
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [snapshot, setSnapshot] = useState<ModelDefaultsSnapshot | null>(null);
  const [drafts, setDrafts] = useState<Record<string, ProviderDraft>>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadStatus, setLoadStatus] = useState<number | null>(null);
  const [loadCode, setLoadCode] = useState<string | undefined>();
  const [saveError, setSaveError] = useState<string | null>(null);
  const [conflictRevision, setConflictRevision] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(true);

  const applySnapshot = useCallback((next: ModelDefaultsSnapshot) => {
    setSnapshot(next);
    setSaved(false);
    setDrafts(draftsFrom(next));
    setConflictRevision(null);
    setSaveError(null);
    setLoadError(null);
    setLoadStatus(null);
    setLoadCode(undefined);
  }, []);

  const load = useCallback(
    async (force: boolean) => {
      setLoading(true);
      try {
        const next = await api.modelDefaults();
        if (force) applySnapshot(next);
        else {
          setSnapshot((current) => current ?? next);
          setDrafts((current) =>
            Object.keys(current).length ? current : draftsFrom(next),
          );
          setLoadError(null);
          setLoadStatus(null);
        }
      } catch (err) {
        const apiErr = err instanceof ApiError ? err : null;
        setLoadError(apiErr?.message ?? "Could not load model defaults.");
        setLoadStatus(apiErr?.status ?? null);
        setLoadCode(apiErr?.code);
      } finally {
        setLoading(false);
      }
    },
    [applySnapshot],
  );

  useEffect(() => {
    void load(true);
  }, [load]);

  const built = useMemo(
    () => configFrom(snapshot?.providers ?? [], snapshot?.roles ?? [], drafts),
    [snapshot, drafts],
  );
  const dirty = snapshot
    ? canonicalJson(built) !== canonicalJson(snapshot.config)
    : false;
  // Read-only board, or not signed in as the operator (CAD-313).
  const gate = useContext(WriteGate);
  const readOnly = Boolean(snapshot?.read_only) || gate !== null;
  const frozen = saving || loading || readOnly;

  const problems = useMemo(() => {
    const found: string[] = [];
    for (const provider of snapshot?.providers ?? []) {
      const draft = drafts[provider.id];
      if (!provider.eligible || !draft?.present) continue;
      if (draft.baseline === "model") {
        const problem = modelIdProblem(draft.baselineModel);
        if (problem) found.push(`${provider.label} baseline: ${problem}`);
      }
      for (const role of snapshot?.roles ?? []) {
        const row = draft.roles[role.id];
        if (row?.choice === "model") {
          const problem = modelIdProblem(row.model);
          if (problem)
            found.push(`${provider.label} ${role.label}: ${problem}`);
        }
      }
    }
    return found;
  }, [snapshot, drafts]);

  function update(id: string, change: (draft: ProviderDraft) => ProviderDraft) {
    setDrafts((current) => {
      const draft = current[id];
      if (!draft) return current;
      return {
        ...current,
        [id]: change({ ...draft, present: true, roles: { ...draft.roles } }),
      };
    });
    setSaveError(null);
    setSaved(false);
  }

  async function save() {
    if (!snapshot || frozen || !dirty || problems.length) return;
    setSaving(true);
    setSaveError(null);
    try {
      const next = await api.saveModelDefaults({
        expected_revision: snapshot.revision,
        config: built,
      });
      applySnapshot(next);
      setSaved(true);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 409 || apiErr?.code === "revision_conflict") {
        setConflictRevision(apiErr.revision ?? null);
        setSaveError(
          "The server revision changed. This draft is still here. Reload discards it.",
        );
      } else {
        setSaveError(apiErr?.message ?? "Save failed. The draft is unchanged.");
      }
    } finally {
      setSaving(false);
    }
  }

  const banner = statusCopy(loadStatus ?? 0, loadCode);
  const provider =
    snapshot?.providers.find((item) => item.id === selectedProvider) ??
    snapshot?.providers[0];
  const draft = provider ? drafts[provider.id] : undefined;
  const listId = provider ? `suggestions-${provider.id}` : undefined;
  const changedProviders =
    snapshot?.providers.filter(
      (item) =>
        canonicalJson(built.providers[item.id]) !==
        canonicalJson(snapshot.config.providers[item.id]),
    ) ?? [];

  function discard() {
    if (!snapshot || frozen) return;
    setDrafts(draftsFrom(snapshot));
    setSaveError(null);
    setConflictRevision(null);
    setSaved(false);
  }

  function resetProvider() {
    if (!provider || frozen) return;
    setDrafts((current) => ({
      ...current,
      [provider.id]: {
        present: false,
        baseline: "provider_default",
        baselineModel: "",
        roles: Object.fromEntries(
          (snapshot?.roles ?? []).map((role) => [role.id, roleDraft()]),
        ),
      },
    }));
    setSaveError(null);
    setSaved(false);
  }

  return (
    <section className="model-defaults-page" aria-busy={loading || saving}>
      <header className="model-defaults-heading">
        <div>
          <h1>Model defaults</h1>
          <p>
            Choose the default model for new agents across all projects.
            Existing agents keep their saved model.
          </p>
        </div>
        {snapshot && (
          <span className="model-defaults-revision">
            Saved revision {snapshot.revision}
          </span>
        )}
      </header>

      {loading && !snapshot && (
        <p role="status" className="text-secondary text-ink-400">
          Loading model defaults…
        </p>
      )}

      {loadError && (
        <div
          className={`model-defaults-notice ${banner ? "model-defaults-warning" : "model-defaults-error"}`}
          role="alert"
        >
          <div>
            <p>{banner || "Could not load model defaults."}</p>
            <p className="text-secondary text-ink-400">{loadError}</p>
          </div>
          <Button
            loading={loading}
            icon={<IconRefresh />}
            onClick={() => void load(true)}
          >
            Retry
          </Button>
        </div>
      )}

      {snapshot && !banner && (
        <>
          {readOnly && (
            <p className="model-defaults-access">
              {snapshot.read_only || gate === READ_ONLY_REASON
                ? "This board is read-only. You can inspect model defaults here."
                : "Sign in using the top bar to edit model defaults."}
            </p>
          )}

          {conflictRevision !== null && (
            <div
              className="model-defaults-notice model-defaults-warning"
              role="alert"
            >
              <div>
                <p>{saveError}</p>
                <p className="text-secondary text-ink-400">
                  Current server revision: {conflictRevision}. Reloading
                  replaces all unsaved provider changes.
                </p>
              </div>
              <Button
                disabled={saving}
                loading={loading}
                onClick={() => void load(true)}
              >
                Reload server copy
              </Button>
            </div>
          )}
          {saveError && conflictRevision === null && (
            <p
              className="model-defaults-notice model-defaults-error"
              role="alert"
            >
              {saveError}
            </p>
          )}
          {problems.length > 0 && (
            <ul
              className="model-defaults-notice model-defaults-error model-defaults-problems"
              aria-label="Model validation errors"
            >
              {problems.map((problem) => (
                <li key={problem}>{problem}</li>
              ))}
            </ul>
          )}

          {snapshot.providers.length === 0 ? (
            <div className="card model-defaults-empty">
              <h2>No model providers available</h2>
              <p>The server returned no providers to configure.</p>
              <Button
                loading={loading}
                icon={<IconRefresh />}
                onClick={() => void load(true)}
              >
                Refresh providers
              </Button>
            </div>
          ) : (
            <div className="model-defaults-workspace">
              <nav
                className="model-defaults-providers"
                aria-label="Model providers"
              >
                <p className="slabel">Provider</p>
                {snapshot.providers.map((item) => {
                  const changed = changedProviders.some(
                    (row) => row.id === item.id,
                  );
                  return (
                    <button
                      type="button"
                      key={item.id}
                      className="model-defaults-provider"
                      aria-pressed={provider?.id === item.id}
                      aria-controls="model-provider-editor"
                      onClick={() => setSelectedProvider(item.id)}
                    >
                      <span>{item.label}</span>
                      {changed ? (
                        <span className="model-provider-state">Unsaved</span>
                      ) : !item.eligible ? (
                        <span className="model-provider-state">
                          Unavailable
                        </span>
                      ) : null}
                    </button>
                  );
                })}
                <p className="model-defaults-nav-hint">
                  Switching providers keeps your draft.
                </p>
              </nav>

              {provider && draft && (
                <article
                  className="card model-defaults-editor"
                  id="model-provider-editor"
                  aria-labelledby="model-provider-title"
                >
                  <header className="model-provider-heading">
                    <h2 id="model-provider-title">{provider.label}</h2>
                    <p>
                      {provider.eligible
                        ? "Set a default model, then override it for individual team roles when needed."
                        : provider.limitation ||
                          "Model selection is unavailable for this provider."}
                    </p>
                  </header>
                  {provider.eligible && (
                    <>
                      <datalist id={listId}>
                        {provider.suggestions.map((model) => (
                          <option key={model} value={model} />
                        ))}
                      </datalist>
                      <div className="model-defaults-baseline">
                        <div>
                          <label
                            htmlFor={`${provider.id}-baseline`}
                            className="model-defaults-label"
                          >
                            Default model
                          </label>
                          <p className="model-defaults-help">
                            Used by roles that inherit this default.
                          </p>
                        </div>
                        <div className="model-defaults-fields">
                          <Select
                            id={`${provider.id}-baseline`}
                            aria-label={`${provider.label} default model`}
                            disabled={frozen}
                            full
                            value={draft.baseline}
                            onChange={(baseline) =>
                              update(provider.id, (row) => ({
                                ...row,
                                baseline: baseline as BaselineChoice,
                              }))
                            }
                            options={[
                              {
                                value: "provider_default",
                                label: "Provider-native default",
                              },
                              { value: "model", label: "Specific model" },
                            ]}
                          />
                          {draft.baseline === "model" && (
                            <div className="model-defaults-model-input">
                              <label htmlFor={`${provider.id}-baseline-model`}>
                                {provider.label} default model ID
                              </label>
                              <input
                                id={`${provider.id}-baseline-model`}
                                className="field"
                                list={listId}
                                disabled={frozen}
                                placeholder="Enter a model ID"
                                value={draft.baselineModel}
                                onChange={(event) =>
                                  update(provider.id, (row) => ({
                                    ...row,
                                    baselineModel: event.target.value,
                                  }))
                                }
                              />
                            </div>
                          )}
                        </div>
                      </div>
                      <section
                        className="model-defaults-roles"
                        aria-labelledby="model-role-title"
                      >
                        <h3 id="model-role-title">Team role overrides</h3>
                        <p className="model-defaults-help">
                          Inherit uses the default above. Provider-native
                          default lets the provider choose, even when you set a
                          specific default.
                        </p>
                        <div className="model-defaults-role-list">
                          {snapshot.roles.map((role) => {
                            const row = draft.roles[role.id] ?? roleDraft();
                            return (
                              <div
                                key={role.id}
                                className="model-defaults-role"
                              >
                                <label
                                  className="model-defaults-label"
                                  htmlFor={`${provider.id}-${role.id}`}
                                >
                                  {role.label}
                                </label>
                                <div className="model-defaults-fields">
                                  <Select
                                    id={`${provider.id}-${role.id}`}
                                    aria-label={`${provider.label} ${role.label} model`}
                                    disabled={frozen}
                                    full
                                    value={row.choice}
                                    onChange={(choice) =>
                                      update(provider.id, (current) => ({
                                        ...current,
                                        roles: {
                                          ...current.roles,
                                          [role.id]: {
                                            ...row,
                                            choice: choice as RoleChoice,
                                          },
                                        },
                                      }))
                                    }
                                    options={[
                                      {
                                        value: "inherit",
                                        label: "Inherit default model",
                                      },
                                      {
                                        value: "provider_default",
                                        label: "Provider-native default",
                                      },
                                      {
                                        value: "model",
                                        label: "Specific model",
                                      },
                                    ]}
                                  />
                                  {row.choice === "model" && (
                                    <div className="model-defaults-model-input">
                                      <label
                                        htmlFor={`${provider.id}-${role.id}-model`}
                                      >
                                        {provider.label} {role.label} model ID
                                      </label>
                                      <input
                                        id={`${provider.id}-${role.id}-model`}
                                        className="field"
                                        list={listId}
                                        disabled={frozen}
                                        placeholder="Enter a model ID"
                                        value={row.model}
                                        onChange={(event) =>
                                          update(provider.id, (current) => ({
                                            ...current,
                                            roles: {
                                              ...current.roles,
                                              [role.id]: {
                                                ...row,
                                                model: event.target.value,
                                              },
                                            },
                                          }))
                                        }
                                      />
                                    </div>
                                  )}
                                </div>
                              </div>
                            );
                          })}
                        </div>
                      </section>
                      <footer className="model-provider-footer">
                        <p className="model-defaults-help">
                          {provider.suggestions_note}
                        </p>
                        <Button
                          variant="ghost"
                          disabled={frozen || !draft.present}
                          onClick={resetProvider}
                        >
                          Reset {provider.label} defaults
                        </Button>
                      </footer>
                    </>
                  )}
                </article>
              )}
            </div>
          )}

          {snapshot.providers.length > 0 && (
            <footer className="model-defaults-savebar">
              <p role="status">
                {saving
                  ? "Saving defaults…"
                  : dirty
                    ? `Unsaved changes in ${changedProviders.length} provider${changedProviders.length === 1 ? "" : "s"}. Save applies all provider changes.`
                    : saved
                      ? "Model defaults saved."
                      : "No unsaved changes."}
              </p>
              <div className="model-defaults-actions">
                <Button disabled={frozen || !dirty} onClick={discard}>
                  Discard changes
                </Button>
                <Button
                  variant="primary"
                  disabled={frozen || !dirty || problems.length > 0}
                  loading={saving}
                  onClick={() => void save()}
                >
                  Save defaults
                </Button>
              </div>
            </footer>
          )}
        </>
      )}
    </section>
  );
}
