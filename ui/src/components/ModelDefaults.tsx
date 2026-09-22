import { useCallback, useEffect, useMemo, useState } from "react";
import { api, ApiError } from "../api";
import { canonicalJson } from "../modelDefaultsCompare";
import { modelIdProblem } from "../modelId";
import type {
  ModelDefaultsConfig,
  ModelDefaultsSnapshot,
  ModelProviderInfo,
  ModelRoleInfo,
  ProviderModelDefaults,
} from "../types";

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
      baseline:
        stored?.default.mode === "model" ? "model" : "provider_default",
      baselineModel:
        stored?.default.mode === "model" ? stored.default.model ?? "" : "",
      roles,
    };
  }
  return out;
}

function selector(choice: BaselineChoice | RoleChoice, model: string) {
  if (choice === "model") return { mode: "model" as const, model: model.trim() };
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
    () =>
      configFrom(
        snapshot?.providers ?? [],
        snapshot?.roles ?? [],
        drafts,
      ),
    [snapshot, drafts],
  );
  const dirty = snapshot ? canonicalJson(built) !== canonicalJson(snapshot.config) : false;
  const readOnly = Boolean(snapshot?.read_only);
  const frozen = saving || readOnly;

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
          if (problem) found.push(`${provider.label} ${role.label}: ${problem}`);
        }
      }
    }
    return found;
  }, [snapshot, drafts]);

  function update(id: string, change: (draft: ProviderDraft) => ProviderDraft) {
    setDrafts((current) => {
      const draft = current[id];
      if (!draft) return current;
      return { ...current, [id]: change({ ...draft, present: true, roles: { ...draft.roles } }) };
    });
    setSaveError(null);
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

  return (
    <section className="px-4 lg:px-8 py-6 space-y-4 max-w-5xl">
      <header className="space-y-1">
        <h1 className="text-xl font-semibold tracking-[-.03em] text-ink-100">
          Model defaults
        </h1>
        <p className="text-secondary text-ink-400">
          Applies to new agents across all projects. The project filter does
          not change this page. Existing agents keep the model saved on their
          row.
        </p>
      </header>

      {loading && !snapshot && (
        <p className="text-secondary text-ink-400">Loading settings…</p>
      )}

      {banner && (
        <div className="card border-warn/40 px-4 py-3 text-secondary text-warn" role="status">
          {banner}
          {loadError ? <div className="mt-1 text-ink-300">{loadError}</div> : null}
        </div>
      )}

      {!banner && loadError && (
        <div className="card border-fail/40 px-4 py-3 text-secondary text-fail" role="alert">
          {loadError}
        </div>
      )}

      {snapshot && !banner && (
        <>
          <div className="flex flex-wrap items-center gap-2 text-secondary">
            <span className="chip bg-ink-800 text-ink-300">
              revision {snapshot.revision}
            </span>
            {snapshot.revision === 0 &&
              Object.keys(snapshot.config.providers).length === 0 && (
                <span className="text-ink-400">
                  No provider baselines are stored yet. New supported agents
                  use each provider&apos;s native default.
                </span>
              )}
            {readOnly && (
              <span className="chip bg-warn/10 text-warn">read-only board</span>
            )}
            {dirty && <span className="chip bg-accent/15 text-accent">unsaved draft</span>}
            {saving && <span className="text-ink-400">Saving…</span>}
          </div>

          {conflictRevision !== null && (
            <div className="card border-warn/40 px-4 py-3 space-y-2" role="alert">
              <p className="text-secondary text-warn">{saveError}</p>
              <p className="text-secondary text-ink-300">
                Current server revision: {conflictRevision}.
              </p>
              <button
                type="button"
                className="h-9 px-3 rounded bg-ink-800 text-ink-100 text-secondary"
                onClick={() => void load(true)}
              >
                Reload server copy
              </button>
            </div>
          )}

          {saveError && conflictRevision === null && (
            <div className="card border-fail/40 px-4 py-3 text-secondary text-fail" role="alert">
              {saveError}
            </div>
          )}

          {problems.length > 0 && (
            <ul className="card border-fail/40 px-4 py-3 text-secondary text-fail space-y-1">
              {problems.map((problem) => (
                <li key={problem}>{problem}</li>
              ))}
            </ul>
          )}

          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              className="h-9 px-3 rounded bg-accent text-ink-950 text-secondary font-medium disabled:opacity-40"
              disabled={frozen || !dirty || problems.length > 0}
              onClick={() => void save()}
            >
              Save defaults
            </button>
            <button
              type="button"
              className="h-9 px-3 rounded bg-ink-800 text-ink-100 text-secondary disabled:opacity-40"
              disabled={frozen || !dirty}
              onClick={() => snapshot && setDrafts(draftsFrom(snapshot))}
            >
              Cancel
            </button>
          </div>

          <div className="space-y-4">
            {snapshot.providers.map((provider) => {
              const draft = drafts[provider.id];
              if (!draft) return null;
              const listId = `suggestions-${provider.id}`;
              return (
                <article key={provider.id} className="card p-4 space-y-3">
                  <div className="flex flex-wrap items-baseline gap-2">
                    <h2 className="text-label font-medium text-ink-100">{provider.label}</h2>
                    <span className="num text-micro text-ink-500">
                      {provider.kinds.join(", ") || "no endpoint"}
                    </span>
                  </div>
                  {!provider.eligible && (
                    <p className="text-secondary text-ink-400">
                      {provider.limitation ??
                        "Model selection is unsupported for this provider."}
                    </p>
                  )}
                  {provider.eligible && (
                    <>
                      <p className="text-micro text-ink-500">{provider.suggestions_note}</p>
                      <datalist id={listId}>
                        {provider.suggestions.map((model) => (
                          <option key={model} value={model} />
                        ))}
                      </datalist>
                      <div className="grid gap-2 sm:grid-cols-[12rem_1fr] sm:items-center">
                        <label className="slabel" htmlFor={`${provider.id}-baseline`}>
                          Baseline
                        </label>
                        <div className="flex flex-wrap gap-2">
                          <select
                            id={`${provider.id}-baseline`}
                            className="field"
                            disabled={frozen}
                            value={draft.baseline}
                            onChange={(event) =>
                              update(provider.id, (row) => ({
                                ...row,
                                baseline: event.target.value as BaselineChoice,
                              }))
                            }
                          >
                            <option value="provider_default">Provider-native default</option>
                            <option value="model">Specific model</option>
                          </select>
                          {draft.baseline === "model" && (
                            <input
                              id={`${provider.id}-baseline-model`}
                              className="field min-w-48"
                              list={listId}
                              disabled={frozen}
                              aria-label={`${provider.label} baseline model`}
                              placeholder="previously observed model"
                              value={draft.baselineModel}
                              onChange={(event) =>
                                update(provider.id, (row) => ({
                                  ...row,
                                  baselineModel: event.target.value,
                                }))
                              }
                            />
                          )}
                        </div>
                      </div>
                      <div className="overflow-x-auto">
                        <table className="w-full text-left text-secondary">
                          <thead>
                            <tr className="text-micro text-ink-500">
                              <th className="py-1 pr-3 font-medium">Team role</th>
                              <th className="py-1 pr-3 font-medium">Model</th>
                            </tr>
                          </thead>
                          <tbody>
                            {snapshot.roles.map((role) => {
                              const row = draft.roles[role.id] ?? roleDraft();
                              return (
                                <tr key={role.id} className="border-t border-ink-800">
                                  <td className="py-2 pr-3 text-ink-200">{role.label}</td>
                                  <td className="py-2">
                                    <div className="flex flex-wrap gap-2">
                                      <label className="sr-only" htmlFor={`${provider.id}-${role.id}`}>
                                        {provider.label} {role.label} model
                                      </label>
                                      <select
                                        id={`${provider.id}-${role.id}`}
                                        className="field"
                                        disabled={frozen}
                                        value={row.choice}
                                        onChange={(event) =>
                                          update(provider.id, (current) => ({
                                            ...current,
                                            roles: {
                                              ...current.roles,
                                              [role.id]: {
                                                ...row,
                                                choice: event.target.value as RoleChoice,
                                              },
                                            },
                                          }))
                                        }
                                      >
                                        <option value="inherit">Inherit provider baseline</option>
                                        <option value="provider_default">Provider-native default</option>
                                        <option value="model">Specific model</option>
                                      </select>
                                      {row.choice === "model" && (
                                        <input
                                          className="field min-w-48"
                                          list={listId}
                                          disabled={frozen}
                                          aria-label={`${provider.label} ${role.label} model id`}
                                          placeholder="previously observed model"
                                          value={row.model}
                                          onChange={(event) =>
                                            update(provider.id, (current) => ({
                                              ...current,
                                              roles: {
                                                ...current.roles,
                                                [role.id]: { ...row, model: event.target.value },
                                              },
                                            }))
                                          }
                                        />
                                      )}
                                    </div>
                                  </td>
                                </tr>
                              );
                            })}
                          </tbody>
                        </table>
                      </div>
                      <button
                        type="button"
                        className="h-8 px-2.5 rounded bg-ink-800 text-ink-200 text-label disabled:opacity-40"
                        disabled={frozen || !draft.present}
                        onClick={() =>
                          setDrafts((current) => ({
                            ...current,
                            [provider.id]: {
                              present: false,
                              baseline: "provider_default",
                              baselineModel: "",
                              roles: Object.fromEntries(
                                (snapshot.roles ?? []).map((role) => [role.id, roleDraft()]),
                              ),
                            },
                          }))
                        }
                      >
                        Reset {provider.label} to inherit
                      </button>
                    </>
                  )}
                </article>
              );
            })}
          </div>
        </>
      )}
    </section>
  );
}
