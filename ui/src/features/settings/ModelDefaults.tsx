import { useContext, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { READ_ONLY_REASON } from "../auth/gate";
import { WriteGate } from "../auth/WriteGate";
import { api, ApiError } from "../../lib/api";
import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { canonicalJson } from "./modelDefaultsCompare";
import "./agentsSettings.css";
import type {
  AgentCatalog,
  AgentCatalogModel,
  AgentHarness,
  AgentModel,
  AgentPreviewResult,
  AgentProvider,
  AgentProviderKind,
  AgentScope,
  AgentSettingsConfig,
  AgentSettingsObservations,
  AgentSettingsSnapshot,
} from "../../lib/types";

const ROLES: Array<[string, string]> = [
  ["pm", "PM"],
  ["research", "Research"],
  ["architect", "Architect"],
  ["dev", "Developer"],
  ["qa", "QA"],
  ["devops", "DevOps"],
  ["worker", "Worker"],
];
const SCOPES: Array<[AgentScope, string]> = [
  ["off", "Not allowed"],
  ["allow", "Allow — all roles"],
  ["master", "Master only"],
  ["worker", "Worker roles only"],
];
const KINDS: AgentProviderKind[] = ["Pi extension", "native CLI"];

const UI_LOAD = "Could not load agent settings.";
const UI_SAVE = "Save failed. The draft is unchanged.";
const UI_CONFLICT = "The server revision changed. This draft is still here. Reload discards it.";

type Tab = "harnesses" | "profiles" | "models";
type Drawer =
  | { kind: "harness"; id: string | null }
  | { kind: "provider"; id: string | null }
  | { kind: "model"; id: string | null; providerId: string; idDraft?: string }
  | { kind: "profile"; id: string; originId: string; idDraft?: string }
  | { kind: "preview"; profileId: string; harnessId: string };

function emptyConfig(): AgentSettingsConfig {
  return { harnesses: [], providers: [], models: [], profiles: [], scope: {}, roleDefaults: {}, pins: [] };
}

function normalize(config: AgentSettingsConfig | null | undefined): AgentSettingsConfig {
  const base = config ?? emptyConfig();
  return {
    harnesses: base.harnesses ?? [],
    providers: base.providers ?? [],
    models: base.models ?? [],
    profiles: base.profiles ?? [],
    scope: base.scope ?? {},
    roleDefaults: base.roleDefaults ?? {},
    pins: base.pins ?? [],
  };
}

function emptyObs(): AgentSettingsObservations {
  return { harnesses: {}, models: {}, profiles: {} };
}

function slug(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

function fact(value: boolean | null | undefined): string {
  if (value === true) return "yes";
  if (value === false) return "no";
  return "unknown";
}

function ago(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const seconds = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

function providerOf(config: AgentSettingsConfig, modelId: string): AgentProvider | undefined {
  const model = config.models.find((item) => item.id === modelId);
  return model ? config.providers.find((item) => item.id === model.provider) : undefined;
}

function allowed(config: AgentSettingsConfig, modelId: string): boolean {
  const provider = providerOf(config, modelId);
  if (!provider?.gate) return true;
  const scope = config.scope[modelId] ?? "off";
  return scope === "allow" || scope === "worker";
}

/** Mockup "serves" column: which providers a harness can launch on, derived from the draft. */
function servesOf(config: AgentSettingsConfig, harness: AgentHarness): string {
  if (harness.id === "pi") {
    const extensions = config.providers.filter((item) => item.kind === "Pi extension").length;
    return extensions ? `${extensions} providers` : "—";
  }
  const own = config.providers.find((item) => item.id === harness.id && item.kind === "native CLI");
  return own ? own.label : "—";
}

function problemsOf(config: AgentSettingsConfig): string[] {
  const found: string[] = [];
  const seen = (label: string, ids: string[]) => {
    const bag = new Set<string>();
    for (const id of ids) {
      if (!id.trim()) found.push(`${label} is missing an id`);
      else if (bag.has(id)) found.push(`Duplicate ${label.toLowerCase()} id “${id}”`);
      else bag.add(id);
    }
  };
  seen("Harness", config.harnesses.map((item) => item.id));
  seen("Provider", config.providers.map((item) => item.id));
  seen("Model", config.models.map((item) => item.id));
  seen("Profile", config.profiles.map((item) => item.id));
  for (const harness of config.harnesses) {
    if (!harness.label.trim()) found.push(`Harness “${harness.id || "new"}” needs a label`);
    if (!harness.cli.trim()) found.push(`Harness “${harness.id || "new"}” needs a registered adapter name`);
  }
  for (const provider of config.providers) {
    if (!provider.label.trim()) found.push(`Provider “${provider.id || "new"}” needs a label`);
    if (!KINDS.includes(provider.kind)) found.push(`${provider.label || provider.id}: kind must be a supported provider kind`);
    if (provider.kind === "native CLI" && provider.gate) found.push(`${provider.label}: native CLI models are not on the Pi allow list`);
  }
  for (const model of config.models) {
    if (!config.providers.some((item) => item.id === model.provider)) found.push(`Model “${model.id}” has no provider`);
  }
  for (const profile of config.profiles) {
    if (!ROLES.some(([id]) => id === profile.role)) found.push(`${profile.id}: role “${profile.role}” is not a team role`);
    if (profile.model !== "inherit" && !config.models.some((item) => item.id === profile.model)) {
      found.push(`${profile.id}: model “${profile.model}” no longer exists`);
    } else if (profile.model !== "inherit" && !allowed(config, profile.model)) {
      found.push(`${profile.id}: “${profile.model}” is not enabled for worker agents`);
    }
  }
  for (const [role, model] of Object.entries(config.roleDefaults)) {
    if (!model) continue;
    if (!config.models.some((item) => item.id === model)) found.push(`Role default ${role}: “${model}” no longer exists`);
  }
  for (const pin of config.pins) {
    if (!pin.pkg.trim() || !pin.pinned.trim()) found.push("A package pin needs both a package and a pinned version");
  }
  return found;
}

function fileBadge(files: Array<{ status: string }> | undefined): { text: string; cls: string } {
  if (!files || !files.length) return { text: "no file status", cls: "plain" };
  if (files.some((file) => file.status === "unmanaged")) return { text: "unmanaged file", cls: "b-warn" };
  if (files.some((file) => file.status === "needs sync")) {
    const count = files.filter((file) => file.status === "needs sync").length;
    return { text: `${count} file${count > 1 ? "s" : ""} need sync`, cls: "b-warn" };
  }
  if (files.some((file) => file.status === "not written")) return { text: "files not written yet", cls: "plain" };
  if (files.every((file) => file.status === "in sync")) return { text: "files in sync", cls: "b-ok" };
  return { text: "file status unknown", cls: "plain" };
}

function activate(event: KeyboardEvent, run: () => void) {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    run();
  }
}

function ModelIdCombo({
  value,
  disabled,
  placeholder,
  catalog,
  knownIds,
  onValue,
  onPick,
}: {
  value: string;
  disabled: boolean;
  placeholder: string;
  catalog: AgentCatalog | undefined;
  knownIds: string[];
  onValue: (id: string) => void;
  onPick: (hit: AgentCatalogModel) => void;
}) {
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(0);
  const matches = useMemo(() => {
    const query = value.trim().toLowerCase();
    return (catalog?.models ?? []).filter((item) => !query || item.id.toLowerCase().includes(query));
  }, [catalog, value]);
  const exact = (catalog?.models ?? []).find((item) => item.id === value.trim());
  const hint = exact
    ? `known to ${catalog?.source} · ${catalog ? ago(catalog.fetchedAt) : ""}`
    : value.trim()
      ? "not in the catalog — the id and details stay as typed"
      : `type an id, or pick one from ${catalog?.source || "the host catalog"}`;

  function close() {
    setOpen(false);
  }

  function pick(index: number) {
    const hit = matches[index];
    if (!hit) return;
    onPick(hit);
    close();
  }

  return (
    <div className="fld">
      <span className="slabel" id="agent-model-id-label">id</span>
      <div className="combo">
        <input
          className="field"
          role="combobox"
          aria-expanded={open}
          aria-autocomplete="list"
          aria-controls="agent-model-catalog"
          aria-labelledby="agent-model-id-label"
          value={value}
          disabled={disabled}
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          onFocus={() => {
            setCursor(0);
            setOpen(true);
          }}
          onChange={(event) => {
            onValue(event.target.value);
            setCursor(0);
            setOpen(true);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              if (!open) {
                setOpen(true);
                return;
              }
              setCursor((current) => {
                const next = current + (event.key === "ArrowDown" ? 1 : -1);
                return Math.max(0, Math.min(matches.length - 1, next));
              });
            } else if (event.key === "Enter" && open) {
              event.preventDefault();
              pick(cursor);
            } else if (event.key === "Escape" && open) {
              event.preventDefault();
              event.stopPropagation();
              close();
            }
          }}
          onBlur={() => {
            window.setTimeout(close, 120);
          }}
        />
        {open && <div className="select-pop">
          <ul className="select-list" id="agent-model-catalog" role="listbox">
            {matches.length === 0 ? (
              <li className="select-empty">{catalog ? "no model matches" : "no catalog for this provider"}</li>
            ) : (
              matches.map((item, index) => (
                <li
                  key={item.id}
                  className="select-opt"
                  role="option"
                  data-active={String(index === cursor)}
                  aria-selected={item.id === value.trim()}
                  onMouseDown={(event) => {
                    event.preventDefault();
                    pick(index);
                  }}
                >
                  <span className="select-copy">
                    <span className="select-label">{item.id}</span>
                    <span className="select-hint">
                      {item.ctx} · out {item.out} · thinking {fact(item.thinking)} · images {fact(item.images)}
                    </span>
                  </span>
                  <span className="select-badge">{knownIds.includes(item.id) ? "added" : "new"}</span>
                </li>
              ))
            )}
          </ul>
        </div>}
      </div>
      <p className="hint">{hint}</p>
    </div>
  );
}

export default function ModelDefaults() {
  const t = useTranslate();
  const gate = useContext(WriteGate);
  const [tab, setTab] = useState<Tab>("harnesses");
  const [saved, setSaved] = useState<AgentSettingsSnapshot | null>(null);
  const [draft, setDraft] = useState<AgentSettingsConfig>(emptyConfig());
  const [observations, setObservations] = useState<AgentSettingsObservations>(emptyObs());
  const [catalogs, setCatalogs] = useState<Record<string, AgentCatalog>>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [note, setNote] = useState("");
  const [drawer, setDrawer] = useState<Drawer | null>(null);
  const [pendingHarness, setPendingHarness] = useState<AgentHarness | null>(null);
  const [pendingProvider, setPendingProvider] = useState<AgentProvider | null>(null);
  const [pendingModel, setPendingModel] = useState<AgentModel | null>(null);
  const [catalogBusy, setCatalogBusy] = useState<string | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [preview, setPreview] = useState<AgentPreviewResult | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const drawerRef = useRef<HTMLElement>(null);
  const drawerOpen = drawer !== null;

  useEffect(() => {
    if (!drawerOpen) return;
    const origin = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusableControls = () => Array.from(drawerRef.current?.querySelectorAll<HTMLElement>(
      "input:not([disabled]), textarea:not([disabled]), select:not([disabled]), button:not([disabled]), a[href], [tabindex='0']",
    ) ?? []);
    focusableControls()[0]?.focus();
    const keepFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !drawerRef.current?.contains(event.target)) focusableControls()[0]?.focus();
    };
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (event.key === "Escape") {
        event.preventDefault();
        closeDrawer();
      } else if (event.key === "Tab") {
        const items = focusableControls();
        const first = items[0];
        const last = items[items.length - 1];
        if (event.shiftKey && document.activeElement === first && last) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last && first) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("focusin", keepFocus);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("focusin", keepFocus);
      document.removeEventListener("keydown", onKey);
      if (origin?.isConnected) origin.focus();
    };
  }, [drawerOpen]);

  function applySnapshot(next: AgentSettingsSnapshot) {
    setSaved(next);
    setDraft(normalize(next.config));
    setObservations(next.observations ?? emptyObs());
    setCatalogs(next.catalogs ?? {});
    setConflict(false);
    setSaveError(null);
    setLoadError(null);
  }

  useEffect(() => {
    let cancel = false;
    setLoading(true);
    api.agentSettings()
      .then((next) => {
        if (!cancel) applySnapshot(next);
      })
      .catch((err) => {
        if (cancel) return;
        setLoadError(err instanceof ApiError ? err.message : UI_LOAD);
      })
      .finally(() => {
        if (!cancel) setLoading(false);
      });
    return () => {
      cancel = true;
    };
  }, []);

  const invalidId = drawer?.kind === "model" || drawer?.kind === "profile" ? drawer.idDraft : undefined;
  const dirty = (saved ? canonicalJson(draft) !== canonicalJson(normalize(saved.config)) : false) || invalidId !== undefined;
  const readOnly = Boolean(saved?.read_only) || gate !== null;
  const issues = useMemo(() => {
    const found = problemsOf(draft);
    if (invalidId !== undefined) {
      const kind = drawer?.kind === "model" ? "Model" : "Profile";
      found.push(invalidId.trim() ? `${kind} '${invalidId}' already exists.` : `${kind} needs an id.`);
    }
    return found;
  }, [draft, invalidId, drawer?.kind]);
  const frozen = !saved || readOnly || saving || loading || syncBusy;

  /** Sections that differ from the saved revision — the dirty chip's count. */
  const changeCount = saved
    ? (["harnesses", "providers", "models", "profiles", "scope", "roleDefaults", "pins"] as const).filter(
        (key) => canonicalJson(draft[key]) !== canonicalJson(normalize(saved.config)[key]),
      ).length + (invalidId !== undefined ? 1 : 0)
    : 0;

  function edit(change: (config: AgentSettingsConfig) => AgentSettingsConfig) {
    setDraft((current) => change(current));
    setSaveError(null);
  }

  function closeDrawer() {
    setDrawer(null);
    setPendingHarness(null);
    setPendingProvider(null);
    setPendingModel(null);
    setPreview(null);
    setPreviewError(null);
  }

  function openNew(kind: Tab) {
    if (frozen) return;
    if (kind === "harnesses") {
      setPendingHarness({ id: "", label: "", cli: "", note: "" });
      setDrawer({ kind: "harness", id: null });
    } else if (kind === "models") {
      setPendingProvider({ id: "", label: "", kind: "Pi extension", pin: "", gate: true, paid: false, note: "" });
      setDrawer({ kind: "provider", id: null });
    } else {
      let taken = 1;
      while (draft.profiles.some((item) => item.id === `new-profile-${taken}`)) taken += 1;
      const id = `new-profile-${taken}`;
      edit((config) => ({
        ...config,
        profiles: [...config.profiles, { id, role: "worker", model: "inherit", job: "" }],
      }));
      setNote(`${id} created · Save writes it to the registry`);
      setDrawer({ kind: "profile", id, originId: id });
    }
  }

  async function reload() {
    setLoading(true);
    try {
      applySnapshot(await api.agentSettings());
    } catch (err) {
      setLoadError(err instanceof ApiError ? err.message : UI_LOAD);
    } finally {
      setLoading(false);
    }
  }

  async function save() {
    if (!saved || frozen || !dirty || issues.length || conflict) return;
    setSaving(true);
    setSaveError(null);
    try {
      const next = await api.saveAgentSettings({ expected_revision: saved.revision, config: draft });
      applySnapshot(next);
      setNote(`saved · revision ${next.revision} · recorded by operator connection`);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 409) {
        setConflict(true);
        setSaveError(apiErr.message || UI_CONFLICT);
      } else {
        setSaveError(apiErr?.message || UI_SAVE);
      }
    } finally {
      setSaving(false);
    }
  }

  async function refreshCatalog(provider: string) {
    setCatalogBusy(provider);
    try {
      const next = await api.refreshAgentCatalog(provider);
      setCatalogs((current) => ({ ...current, [provider]: next }));
      if (next.error) setNote(next.error);
      else setNote(`Catalog read from ${next.source || provider}.`);
    } catch (err) {
      const message = err instanceof ApiError ? err.message : "Catalog refresh failed.";
      setCatalogs((current) => ({
        ...current,
        [provider]: { source: current[provider]?.source ?? "", fetchedAt: current[provider]?.fetchedAt ?? "", models: current[provider]?.models ?? [], error: message },
      }));
      setNote(message);
    } finally {
      setCatalogBusy(null);
    }
  }

  function importCatalog(provider: AgentProvider) {
    const catalog = catalogs[provider.id];
    if (!catalog || catalog.error) return;
    const fresh = catalog.models.filter((item) => !draft.models.some((model) => model.id === item.id));
    if (!fresh.length) return;
    edit((config) => {
      const models = [...config.models];
      const scope = { ...config.scope };
      for (const item of fresh) {
        models.push({
          id: item.id,
          provider: provider.id,
          ctx: item.ctx,
          out: item.out,
          thinking: item.thinking,
          images: item.images,
        });
        if (provider.gate && !scope[item.id]) scope[item.id] = "off";
      }
      return { ...config, models, scope };
    });
    setNote(`Imported ${fresh.length} into ${provider.label} as not allowed. Enable the ones you want.`);
  }

  async function sync(profile: string | null) {
    if (!saved || dirty || readOnly || syncBusy) return;
    setSyncBusy(true);
    try {
      const result = await api.syncAgentProfiles({ expected_revision: saved.revision, profile });
      applySnapshot(result.snapshot);
      const summary = (result.files ?? []).map((file) => `${file.profile} ${file.path}: ${file.status}`).join(" · ");
      setNote(summary || "Sync finished with no file results.");
    } catch (err) {
      setNote(err instanceof ApiError ? err.message : "Sync failed. Saved files were not changed by the board.");
    } finally {
      setSyncBusy(false);
    }
  }

  useEffect(() => {
    if (drawer?.kind !== "preview" || !saved || dirty) return;
    let cancel = false;
    setPreviewLoading(true);
    setPreview(null);
    setPreviewError(null);
    api.previewAgentLaunch({
      expected_revision: saved.revision,
      profile: drawer.profileId,
      harness: drawer.harnessId,
    })
      .then((result) => {
        if (!cancel) setPreview(result);
      })
      .catch((err) => {
        if (!cancel) setPreviewError(err instanceof ApiError ? err.message : "Preview failed.");
      })
      .finally(() => {
        if (!cancel) setPreviewLoading(false);
      });
    return () => {
      cancel = true;
    };
  }, [drawer, saved, dirty]);

  function refuse(message: string) {
    setNote(message);
  }

  function removeHarness(id: string) {
    const live = observations.harnesses[id]?.live ?? 0;
    if (live > 0) {
      refuse(`“${id}” has ${live} live agent${live === 1 ? "" : "s"}. Removal stays in the draft only after they stop.`);
      return;
    }
    edit((config) => ({ ...config, harnesses: config.harnesses.filter((item) => item.id !== id) }));
    setNote(`${id} removed from the draft.`);
    closeDrawer();
  }

  function removeProvider(id: string) {
    const models = draft.models.filter((item) => item.provider === id);
    const live = models.filter((item) => (observations.models[item.id]?.live ?? 0) > 0);
    const referenced = draft.profiles.filter((profile) => models.some((model) => model.id === profile.model));
    if (live.length || referenced.length) {
      refuse(
        referenced.length
          ? `${id} is still referenced by ${referenced.map((item) => item.id).join(", ")}. Change those profiles first.`
          : `${id} still has live agents. Removal is refused.`,
      );
      return;
    }
    const removed = new Set(models.map((item) => item.id));
    edit((config) => ({
      ...config,
      providers: config.providers.filter((item) => item.id !== id),
      models: config.models.filter((item) => item.provider !== id),
      scope: Object.fromEntries(Object.entries(config.scope).filter(([modelId]) => !removed.has(modelId))) as AgentSettingsConfig["scope"],
    }));
    setNote(`${id} and its models were removed from the draft.`);
    closeDrawer();
  }

  function removeModel(id: string) {
    const users = draft.profiles.filter((profile) => profile.model === id);
    const live = observations.models[id]?.live ?? 0;
    if (users.length || live > 0) {
      refuse(users.length ? `${id} is used by ${users.map((item) => item.id).join(", ")}.` : `${id} has ${live} live agent${live === 1 ? "" : "s"}.`);
      return;
    }
    edit((config) => {
      const scope = { ...config.scope };
      delete scope[id];
      return { ...config, models: config.models.filter((item) => item.id !== id), scope };
    });
    setNote(`${id} removed from the draft.`);
    closeDrawer();
  }

  function removeProfile(id: string, originId = id) {
    const used = observations.profiles[originId]?.usedBy ?? [];
    if (used.length) {
      refuse(`${id} is in use by ${used.join(", ")}. Delete is refused while those references remain.`);
      return;
    }
    edit((config) => ({ ...config, profiles: config.profiles.filter((item) => item.id !== id) }));
    setNote(`${id} removed from the draft. Saved history stays on the server until a successful save.`);
    closeDrawer();
  }

  const sub = {
    harnesses: "The launchers and what they can run.",
    profiles: "Versioned jobs: a description and the model it runs on.",
    models: "Providers, their models, and what is allowed.",
  }[tab];

  function renderHarnesses() {
    return (
      <div>
        <p className="lead">{t("Launchers. A harness starts an agent; a provider supplies the models. Pi launches on the Pi-extension providers — the native CLIs supply their own.")}</p>
        <div className="card">
          {draft.harnesses.map((harness) => {
            const observed = observations.harnesses[harness.id];
            const version = observed?.version ? `${harness.cli} ${observed.version}` : harness.cli;
            const installed = observed?.installed === true ? `${observed.live} live` : observed?.installed === false ? "not installed" : "unknown";
            return (
              <div key={harness.id} className="hrow" role="button" tabIndex={0} aria-label={`Edit harness ${harness.label || harness.id}`} onClick={() => setDrawer({ kind: "harness", id: harness.id })} onKeyDown={(event) => activate(event, () => setDrawer({ kind: "harness", id: harness.id }))}>
                <div className="hname"><b>{harness.label || harness.id}</b><span>{version}</span></div>
                <div className="hnote">{harness.note}</div>
                <div className="hmodels">{servesOf(draft, harness)}</div>
                <div className="hlive">{installed}</div>
              </div>
            );
          })}
          {draft.harnesses.length === 0 && (
            <div className="emptyrow"><span>{t("No harnesses yet.")}</span><Button size="sm" disabled={frozen} onClick={() => openNew("harnesses")}>{t("Add harness")}</Button></div>
          )}
        </div>
        <p className="hint">{t("Recording is a declaration: the version is read from the host, the endpoint kinds come from Cadence's adapter registry.")}</p>
      </div>
    );
  }

  function renderProfiles() {
    return (
      <div>
        <p className="lead">{t("A profile is a job description and the model it runs on, versioned with the project. Sync writes it into the harness files for launches started by hand.")}</p>
        <div className="card">
          {draft.profiles.map((profile) => {
            const observed = observations.profiles[profile.id];
            const badge = fileBadge(observed?.files);
            return (
              <div key={profile.id} className="prow" role="button" tabIndex={0} aria-label={`Edit profile ${profile.id}`} onClick={() => setDrawer({ kind: "profile", id: profile.id, originId: profile.id })} onKeyDown={(event) => activate(event, () => setDrawer({ kind: "profile", id: profile.id, originId: profile.id }))}>
                <div className="pname">
                  <b>{profile.id} {observed?.version ? <span className="vtag">v{observed.version}</span> : <span className="vtag">unsaved</span>}</b>
                  <span>{profile.job}</span>
                </div>
                <div className="pmodel">{profile.model === "inherit" ? "inherit → role default" : profile.model}</div>
                <div className="pfiles"><span className={`badge ${badge.cls}`}>{badge.text}</span></div>
                <div className="pact">
                  <Button size="sm" disabled={frozen || dirty || syncBusy} title={dirty ? "Save before sync" : undefined} onClick={(event) => { event.stopPropagation(); void sync(profile.id); }}>Sync</Button>
                  <Button size="sm" disabled={!saved?.config.profiles.length || dirty} title={dirty ? "Save before preview" : undefined} onClick={(event) => { event.stopPropagation(); setDrawer({ kind: "preview", profileId: profile.id, harnessId: saved?.config.harnesses[0]?.id || "" }); }}>Launch</Button>
                </div>
              </div>
            );
          })}
          {draft.profiles.length === 0 && (
            <div className="emptyrow"><span>{t("No profiles yet.")}</span><Button size="sm" disabled={frozen} onClick={() => openNew("profiles")}>{t("Add profile")}</Button></div>
          )}
        </div>
        <p className="hint">{t("Files exist for hands-on launches only: Cadence-managed launches read the registry, so nothing has to be kept in step. File sync and preview use the last saved revision.")}</p>
      </div>
    );
  }

  function renderModels() {
    const roles = ROLES;
    return (
      <div>
        <p className="lead">{t("Providers first, then the models under each. Toggle a model into the allow list; profiles pick from what is enabled. Native CLIs have no allow list.")}</p>
        <div className="card" style={{ marginBottom: 12 }}>
          <div className="grp">{t("Role defaults")}</div>
          {roles.map(([id, label]) => {
            const defId = draft.roleDefaults[id] ?? "";
            const defModel = defId ? draft.models.find((model) => model.id === defId) : undefined;
            const defProvider = defId ? providerOf(draft, defId) : undefined;
            const defBad = Boolean(defId) && (!defModel || (defProvider?.gate === true && !allowed(draft, defId)));
            const eff = !defId
              ? "unset"
              : !defModel
                ? "—"
                : defProvider?.gate
                  ? allowed(draft, defId)
                    ? (draft.scope[defId] ?? "off")
                    : "not enabled"
                  : "always available";
            return (
              <div className="drow" key={id}>
                <span className="rlabel">{label}</span>
                <select
                  className={defId ? (defBad ? "bad" : "") : "inherit"}
                  aria-label={`${label} default model`}
                  disabled={frozen}
                  value={defId}
                  onChange={(event) => edit((config) => ({ ...config, roleDefaults: { ...config.roleDefaults, [id]: event.target.value } }))}
                >
                  <option value="">unset</option>
                  {draft.models.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}
                  {defId && !defModel && <option value={defId}>{defId}</option>}
                </select>
                <span className="rwhy"><span className="eff">{eff}</span></span>
                {defBad && (
                  <p className="rhint">{!defModel ? `“${defId}” no longer exists` : `“${defId}” is not enabled for worker agents`}</p>
                )}
              </div>
            );
          })}
        </div>
        {draft.providers.map((provider) => {
          const models = draft.models.filter((model) => model.provider === provider.id);
          const on = models.filter((model) => !provider.gate || (draft.scope[model.id] ?? "off") !== "off").length;
          const catalog = catalogs[provider.id];
          const fresh = catalog && !catalog.error ? catalog.models.filter((item) => !draft.models.some((model) => model.id === item.id)) : [];
          return (
            <div className="pcard" key={provider.id}>
              <div className="phead" role="button" tabIndex={0} aria-label={`Edit provider ${provider.label}`} onClick={() => setDrawer({ kind: "provider", id: provider.id })} onKeyDown={(event) => activate(event, () => setDrawer({ kind: "provider", id: provider.id }))}>
                <b>{provider.label}</b>
                <span className="kind">{provider.kind} · {provider.pin || "no pin"}</span>
                {provider.paid && <span className="badge b-warn">paid</span>}
                <span className="count">{provider.gate ? `${on} of ${models.length} in use` : `${models.length} models`}</span>
                <div className="note">{provider.note}</div>
              </div>
              <div className="catline">
                <span className="src">
                  {catalogBusy === provider.id
                    ? "Refreshing catalog…"
                    : catalog?.error
                      ? `catalog failed — ${catalog.error}`
                      : catalog
                        ? `catalog · ${catalog.models.length} models · ${ago(catalog.fetchedAt)} — from ${catalog.source}`
                        : "No catalog yet"}
                </span>
                {fresh.length > 0 && <Button size="sm" disabled={frozen} onClick={() => importCatalog(provider)}>Import {fresh.length} new</Button>}
                <Button size="sm" disabled={catalogBusy === provider.id} loading={catalogBusy === provider.id} onClick={() => void refreshCatalog(provider.id)}>Refresh</Button>
              </div>
              {models.map((model) => {
                const scope = draft.scope[model.id] ?? "off";
                const users = draft.profiles.filter((profile) => profile.model === model.id).map((profile) => profile.id);
                const live = observations.models[model.id]?.live ?? 0;
                return (
                  <div key={model.id} className={`mrow${provider.gate && scope === "off" ? " off" : ""}`} role="button" tabIndex={0} aria-label={`Edit model ${model.id}`} onClick={() => setDrawer({ kind: "model", id: model.id, providerId: provider.id })} onKeyDown={(event) => activate(event, () => setDrawer({ kind: "model", id: model.id, providerId: provider.id }))}>
                    <div className="mid">{model.id}</div>
                    <div className="meta">{model.ctx || "ctx unknown"} · out {model.out || "unknown"}{live ? ` · ${live} live` : ""}</div>
                    {provider.gate ? (
                      <select
                        data-scope={scope}
                        aria-label={`Allow list for ${model.id}`}
                        disabled={frozen}
                        value={scope}
                        onClick={(event) => event.stopPropagation()}
                        onKeyDown={(event) => event.stopPropagation()}
                        onChange={(event) => edit((config) => ({ ...config, scope: { ...config.scope, [model.id]: event.target.value as AgentScope } }))}
                      >
                        {SCOPES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
                      </select>
                    ) : <div className="always">always available</div>}
                    <div className="use">{users.length ? `used by ${users.join(", ")}` : "—"}</div>
                  </div>
                );
              })}
              <div className="addrow">
                <Button size="sm" disabled={frozen} onClick={() => { setPendingModel({ id: "", provider: provider.id, ctx: "", out: "", thinking: null, images: null }); setDrawer({ kind: "model", id: null, providerId: provider.id }); }}>+ Add model</Button>
              </div>
            </div>
          );
        })}
        {draft.providers.length === 0 && (
          <div className="emptyrow"><span>{t("No providers yet.")}</span><Button size="sm" disabled={frozen} onClick={() => openNew("models")}>{t("Add provider")}</Button></div>
        )}
        <details className="card pins">
          <summary>{t("Pinned Pi extensions")} — {draft.pins.length} {t("pinned")}</summary>
          {draft.pins.map((pin, index) => (
            <div className="pinrow" key={`${pin.pkg}-${index}`}>
              <input className="pkg" aria-label="Package" disabled={frozen} value={pin.pkg} onChange={(event) => edit((config) => ({ ...config, pins: config.pins.map((item, itemIndex) => itemIndex === index ? { ...item, pkg: event.target.value } : item) }))} />
              <input className="ver" aria-label="Pinned version" disabled={frozen} value={pin.pinned} onChange={(event) => edit((config) => ({ ...config, pins: config.pins.map((item, itemIndex) => itemIndex === index ? { ...item, pinned: event.target.value } : item) }))} />
              <span className="path">Install path not reported</span>
              <Button size="sm" variant="danger" disabled={frozen} onClick={() => edit((config) => ({ ...config, pins: config.pins.filter((_, itemIndex) => itemIndex !== index) }))}>Remove</Button>
            </div>
          ))}
          {draft.pins.length === 0 && <p className="hint">No package pins in this revision. A pin records a version; it does not install a package.</p>}
          <div className="addrow"><Button size="sm" disabled={frozen} onClick={() => edit((config) => ({ ...config, pins: [...config.pins, { pkg: "", pinned: "" }] }))}>Add pin</Button></div>
        </details>
      </div>
    );
  }

  function field(label: string, node: ReactNode, hint?: string) {
    return <label className="fld"><span className="slabel">{label}</span>{node}{hint ? <p className="hint">{hint}</p> : null}</label>;
  }

  function renderDrawer() {
    if (!drawer) return null;
    let title = "";
    let kicker = "";
    let body: ReactNode = null;
    if (drawer.kind === "harness") {
      const existing = drawer.id ? draft.harnesses.find((item) => item.id === drawer.id) : null;
      const harness = existing ?? pendingHarness ?? { id: "", label: "", cli: "", note: "" };
      const observed = existing ? observations.harnesses[existing.id] : undefined;
      kicker = existing ? "harness" : "new harness";
      title = existing ? existing.label || existing.id : "Add a harness";
      const set = (patch: Partial<AgentHarness>) => {
        if (existing) edit((config) => ({ ...config, harnesses: config.harnesses.map((item) => item.id === existing.id ? { ...item, ...patch } : item) }));
        else setPendingHarness({ ...harness, ...patch });
      };
      body = (
        <>
          <section className="dsec"><div className="dtitle">Identity</div><div className="dinn">
            <div className="grid2">
              {field("label", <input disabled={frozen} value={harness.label} onChange={(event) => set({ label: event.target.value })} />)}
              {field("CLI", <input disabled={frozen} value={harness.cli} onChange={(event) => set({ cli: event.target.value })} />)}
            </div>
            {!existing && field("id", <input disabled={frozen} value={harness.id} placeholder="pi" onChange={(event) => set({ id: event.target.value })} />, "Registered adapter ID, independent of the display label.")}
            <div className="vrow"><span>on this host</span><span className={`badge ${observed?.installed ? "b-ok" : "plain"}`}>{observed?.installed === true ? `installed · ${observed.live} live` : observed?.installed === false ? "not installed" : "unknown"}{observed?.version ? ` · ${observed.version}` : ""}</span></div>
            <p className="hint">Read from the host, not set here — a declared harness that is not installed fences on launch.</p>
          </div></section>
          <section className="dsec"><div className="dinn">
            {field("note", <textarea disabled={frozen} value={harness.note} onChange={(event) => set({ note: event.target.value })} />)}
          </div></section>
          <p className="hint">The endpoint kinds and launch parameters come from Cadence's adapter registry — a provider that is not there needs its adapter before this harness can use it.</p>
          <div className="dact">
            <Button size="sm" variant="primary" disabled={frozen} onClick={() => {
              if (existing) { closeDrawer(); return; }
              const id = slug(harness.id || harness.label);
              if (!id) { setNote("A harness needs an id."); return; }
              edit((config) => ({ ...config, harnesses: [...config.harnesses, { ...harness, id }] }));
              setNote(`${harness.label || id} added to the draft.`);
              closeDrawer();
            }}>{existing ? "Save harness" : "Add harness"}</Button>
            {existing && <Button size="sm" variant="danger" disabled={frozen} onClick={() => removeHarness(existing.id)}>Remove</Button>}
          </div>
        </>
      );
    } else if (drawer.kind === "provider") {
      const existing = drawer.id ? draft.providers.find((item) => item.id === drawer.id) : null;
      const provider = existing ?? pendingProvider ?? { id: "", label: "", kind: "Pi extension" as const, pin: "", gate: true, paid: false, note: "" };
      kicker = existing ? "provider" : "new provider";
      title = existing ? existing.label || existing.id : "Add a provider";
      const set = (patch: Partial<AgentProvider>) => {
        const next = { ...provider, ...patch };
        if (next.kind === "native CLI") next.gate = false;
        if (existing) edit((config) => ({ ...config, providers: config.providers.map((item) => item.id === existing.id ? next : item) }));
        else setPendingProvider(next);
      };
      body = (
        <>
          <section className="dsec"><div className="dtitle">Provider</div><div className="dinn">
            <div className="grid2">
              {field("label", <input disabled={frozen} value={provider.label} onChange={(event) => set({ label: event.target.value })} />)}
              {field("kind", <select disabled={frozen} value={provider.kind} onChange={(event) => set({ kind: event.target.value as AgentProviderKind })}>{KINDS.map((kind) => <option key={kind}>{kind}</option>)}</select>)}
            </div>
            {!existing && field("id", <input disabled={frozen} value={provider.id} onChange={(event) => set({ id: event.target.value })} />, "Provider ID used by model names and the adapter registry.")}
            {field("pin (extension package@version, or CLI version)", <input disabled={frozen} value={provider.pin} placeholder="pi-opencode-go 1.3.0" onChange={(event) => set({ pin: event.target.value })} />)}
            <div className="grid2">
              <label className="check"><input type="checkbox" disabled={frozen || provider.kind !== "Pi extension"} checked={provider.gate} onChange={(event) => set({ gate: event.target.checked })} /><span>gated by the allow list</span></label>
              <label className="check"><input type="checkbox" disabled={frozen} checked={provider.paid} onChange={(event) => set({ paid: event.target.checked })} /><span>paid (excluded for workers by default)</span></label>
            </div>
            {field("note", <textarea disabled={frozen} value={provider.note} onChange={(event) => set({ note: event.target.value })} />)}
          </div></section>
          <p className="hint">A Pi extension provider is what the pi launcher loads with -e; a native CLI provider is served by the harness itself.</p>
          <div className="dact">
            <Button size="sm" variant="primary" disabled={frozen} onClick={() => {
              if (existing) { closeDrawer(); return; }
              const id = slug(provider.id || provider.label);
              if (!id) { setNote("A provider needs an id."); return; }
              const next = { ...provider, id, gate: provider.kind === "native CLI" ? false : provider.gate };
              edit((config) => ({ ...config, providers: [...config.providers, next] }));
              setNote(`${next.label || id} added · add its models next`);
              closeDrawer();
            }}>{existing ? "Save provider" : "Add provider"}</Button>
            {existing && <Button size="sm" variant="danger" disabled={frozen} onClick={() => removeProvider(existing.id)}>Remove</Button>}
          </div>
        </>
      );
    } else if (drawer.kind === "model") {
      const existing = drawer.id ? draft.models.find((item) => item.id === drawer.id) : null;
      const model = existing ?? pendingModel ?? { id: "", provider: drawer.providerId, ctx: "", out: "", thinking: null, images: null };
      const provider = draft.providers.find((item) => item.id === model.provider);
      const catalog = catalogs[model.provider];
      kicker = existing ? "model" : "new model";
      title = existing ? existing.id : `Add a model to ${provider?.label || model.provider}`;
      const set = (patch: Partial<AgentModel>) => {
        if (existing) {
          const nextId = patch.id?.trim() ?? existing.id;
          if (patch.id !== undefined && (!nextId || draft.models.some((item) => item.id === nextId && item !== existing))) {
            setDrawer({ ...drawer, idDraft: patch.id });
            return;
          }
          edit((config) => {
            const models = config.models.map((item) => item.id === existing.id ? { ...item, ...patch, id: nextId } : item);
            if (nextId === existing.id) return { ...config, models };
            const scope = { ...config.scope };
            const previousScope = scope[existing.id];
            delete scope[existing.id];
            if (previousScope !== undefined) scope[nextId] = previousScope;
            return {
              ...config,
              models,
              scope,
              profiles: config.profiles.map((profile) => profile.model === existing.id ? { ...profile, model: nextId } : profile),
              roleDefaults: Object.fromEntries(Object.entries(config.roleDefaults).map(([role, id]) => [role, id === existing.id ? nextId : id])),
            };
          });
          if (patch.id !== undefined) setDrawer({ ...drawer, id: nextId, idDraft: undefined });
        } else setPendingModel({ ...model, ...patch });
      };
      const users = draft.profiles.filter((profile) => profile.model === model.id).map((profile) => profile.id);
      body = (
        <>
          <section className="dsec"><div className="dtitle">Model</div><div className="dinn">
            <ModelIdCombo
              value={drawer.idDraft ?? model.id}
              disabled={frozen}
              placeholder={`${model.provider}/model-name`}
              catalog={catalog}
              knownIds={draft.models.map((item) => item.id)}
              onValue={(id) => set({ id })}
              onPick={(hit) => set({ id: hit.id, ctx: hit.ctx, out: hit.out, thinking: hit.thinking, images: hit.images })}
            />
            <div className="dact">
              <span className="ref">{catalog ? `${catalog.source} · ${ago(catalog.fetchedAt)}` : "no catalog for this provider"}{catalog?.error ? ` — ${catalog.error}` : ""}</span>
              <Button size="sm" disabled={catalogBusy === model.provider} loading={catalogBusy === model.provider} onClick={() => void refreshCatalog(model.provider)}>Refresh</Button>
            </div>
            <div className="grid2">
              {field("context", <input data-field="ctx" disabled={frozen} value={model.ctx} placeholder="262K" onChange={(event) => set({ ctx: event.target.value })} />)}
              {field("max out", <input data-field="out" disabled={frozen} value={model.out} placeholder="128K" onChange={(event) => set({ out: event.target.value })} />)}
            </div>
            <div className="vrow"><span>live agents on it</span><b>{observations.models[model.id]?.live ?? 0}</b></div>
            <p className="hint">Observed from the fleet, not set here. Unknown catalog metadata stays unknown until you pick a catalog row.</p>
            <div className="grid2">
              {field("thinking", <div>{fact(model.thinking)}</div>)}
              {field("images", <div>{fact(model.images)}</div>)}
            </div>
            {provider?.gate ? <div className="grid2">{field("allow list", <select disabled={frozen} value={draft.scope[model.id] ?? "off"} onChange={(event) => edit((config) => ({ ...config, scope: { ...config.scope, [model.id]: event.target.value as AgentScope } }))}>{SCOPES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select>)}</div> : <p className="hint">{provider?.label || "This provider"} has no allow list — its models are always available.</p>}
          </div></section>
          <p className="hint">{users.length ? `Used by ${users.join(", ")}.` : "No profile uses this model yet."}</p>
          <div className="dact">
            <Button size="sm" variant="primary" disabled={frozen} onClick={() => {
              const id = model.id.trim();
              if (!id) { setNote("A model needs an id."); return; }
              if (!existing && draft.models.some((item) => item.id === id)) { setNote(`${id} already exists`); return; }
              if (!id.includes("/")) setNote(`note: “${id}” has no provider prefix`);
              if (!existing) {
                edit((config) => ({
                  ...config,
                  models: [...config.models, { ...model, id }],
                  scope: provider?.gate ? { ...config.scope, [id]: config.scope[id] ?? "off" } : config.scope,
                }));
                setNote(`${id} added to the draft${provider?.gate ? " as not allowed" : ""}.`);
              }
              closeDrawer();
            }}>{existing ? "Save model" : "Add model"}</Button>
            {existing && <Button size="sm" variant="danger" disabled={frozen} onClick={() => removeModel(existing.id)}>Remove</Button>}
          </div>
        </>
      );
    } else if (drawer.kind === "profile") {
      const profile = draft.profiles.find((item) => item.id === drawer.id);
      if (!profile) {
        body = <p className="hint">This profile is not in the draft.</p>;
      } else {
        const observed = observations.profiles[drawer.originId];
        const resolved = profile.model === "inherit" ? draft.roleDefaults[profile.role] || "" : profile.model;
        const blocked = profile.model !== "inherit" && !allowed(draft, profile.model);
        kicker = observed?.version ? `profile · v${observed.version}` : "profile · unsaved";
        title = profile.id;
        body = (
          <>
            <section className="dsec"><div className="dtitle">Job</div><div className="dinn">
              <div className="grid2">
                {field("name", <input disabled={frozen} value={drawer.idDraft ?? profile.id} onChange={(event) => {
                  const id = event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "-");
                  if (!id || draft.profiles.some((item) => item.id === id && item !== profile)) {
                    setDrawer({ ...drawer, idDraft: id });
                    return;
                  }
                  edit((config) => ({ ...config, profiles: config.profiles.map((item) => item.id === profile.id ? { ...item, id } : item) }));
                  setDrawer({ ...drawer, id, idDraft: undefined });
                }} />)}
                {field("role", <select disabled={frozen} value={profile.role} onChange={(event) => edit((config) => ({ ...config, profiles: config.profiles.map((item) => item.id === profile.id ? { ...item, role: event.target.value } : item) }))}>{ROLES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select>, "The role names the job. It does not grant runtime authority.")}
              </div>
              {field("job description", <textarea disabled={frozen} value={profile.job} onChange={(event) => edit((config) => ({ ...config, profiles: config.profiles.map((item) => item.id === profile.id ? { ...item, job: event.target.value } : item) }))} />, "The agent's instruction at launch, and the body of the generated files.")}
            </div></section>
            <section className="dsec"><div className="dtitle">Model</div><div className="dinn">
              {field("model", <select disabled={frozen} value={profile.model} onChange={(event) => edit((config) => ({ ...config, profiles: config.profiles.map((item) => item.id === profile.id ? { ...item, model: event.target.value } : item) }))}>
                <option value="inherit">inherit → role default ({draft.roleDefaults[profile.role] || "unset"})</option>
                {draft.providers.map((provider) => {
                  const rows = draft.models.filter((model) => model.provider === provider.id);
                  if (!rows.length) return null;
                  return <optgroup key={provider.id} label={provider.label}>{rows.map((model) => <option key={model.id} value={model.id}>{model.id}{model.ctx ? ` · ${model.ctx}` : ""}{allowed(draft, model.id) ? "" : "  — not enabled"}</option>)}</optgroup>;
                })}
              </select>)}
              <div className={`verdict${blocked ? " bad" : ""}`}>
                <div className="vmodel">{resolved || "unresolved"}</div>
                <div className="vrow"><span>model from</span><b>{profile.model === "inherit" ? "role default" : "this profile"}</b></div>
                <div className="vrow"><span>provider</span><b>{providerOf(draft, resolved)?.label || "—"}</b></div>
                <div className="vrow"><span>allow list</span><b>{providerOf(draft, resolved)?.gate ? (allowed(draft, resolved) ? `yes · ${draft.scope[resolved] ?? "off"}` : "not enabled") : "n/a"}</b></div>
                {blocked && <p className="verr">“{profile.model}” is not enabled for worker roles — enable it in Providers &amp; models</p>}
              </div>
            </div></section>
            <section className="dsec"><div className="dtitle">Files</div><div className="dinn">
              {(observed?.files ?? []).map((file) => (
                <div className="target" key={file.path}><span className="tpath">{file.path}</span><span className={`badge ${file.status === "in sync" ? "b-ok" : file.status === "needs sync" || file.status === "unmanaged" ? "b-warn" : "plain"}`}>{file.status}</span></div>
              ))}
              {!observed?.files?.length && <p className="hint">No generated-file status for this profile yet. Save, then sync, to write managed files.</p>}
            </div></section>
            <section className="dsec"><div className="dtitle">History{observed?.version ? ` · v${observed.version}` : ""}</div><div className="dinn">
              <div className="hist">
                {(observed?.history ?? []).map((item, index) => (
                  <div className="hrow2" key={`${item.at}-${index}`}><span className="hv">{item.at}</span><span className="hw">{item.what}</span></div>
                ))}
              </div>
              {!observed?.history?.length && <p className="hint">No history recorded for this profile.</p>}
              {!!observed?.usedBy?.length && <p className="hint">In use by {observed.usedBy.join(", ")}.</p>}
            </div></section>
            <div className="dact">
              <Button size="sm" disabled={frozen || dirty || syncBusy} title={dirty ? "Save before sync" : undefined} onClick={() => void sync(profile.id)}>Sync files</Button>
              <Button size="sm" variant="primary" disabled={!saved?.config.profiles.length || dirty} title={dirty ? "Save before preview" : undefined} onClick={() => setDrawer({ kind: "preview", profileId: profile.id, harnessId: saved?.config.harnesses[0]?.id || "" })}>Preview launch</Button>
              <Button size="sm" variant="danger" disabled={frozen} onClick={() => removeProfile(profile.id, drawer.originId)}>Delete profile</Button>
            </div>
          </>
        );
      }
    } else {
      const savedConfig = normalize(saved?.config);
      kicker = "preview";
      title = "What this launch does";
      body = (
        <>
          {dirty && <p className="notice">Preview uses the saved revision. Save or discard the draft first.</p>}
          <div className="grid2">
            {field("run on harness", <select disabled={!saved || dirty} value={drawer.harnessId} onChange={(event) => setDrawer({ ...drawer, harnessId: event.target.value })}>{(dirty ? draft : savedConfig).harnesses.map((item) => { const installed = observations.harnesses[item.id]?.installed; return <option key={item.id} value={item.id}>{item.label || item.id}{installed === true ? " · installed" : installed === false ? " · not installed" : ""}</option>; })}</select>)}
            {field("profile", <select disabled={!saved || dirty} value={drawer.profileId} onChange={(event) => setDrawer({ ...drawer, profileId: event.target.value })}>{(dirty ? draft : savedConfig).profiles.map((item) => <option key={item.id} value={item.id}>{item.id}{observations.profiles[item.id]?.version ? ` · ${observations.profiles[item.id].version}` : ""}</option>)}</select>)}
          </div>
          {previewLoading && <p className="hint">Resolving the saved profile…</p>}
          {previewError && <p className="notice bad" role="alert">{previewError}</p>}
          {preview && (
            <>
              <pre className="launchline">{preview.command || "No command. This combination is not launched."}</pre>
              <div className={`verdict${preview.allowed ? "" : " bad"}`}>
                <div className="vmodel">{preview.model || "unresolved"}</div>
                <div className="vrow"><span>model from</span><b>{preview.source || "—"}</b></div>
                <div className="vrow"><span>provider</span><b>{providerOf(savedConfig, preview.model || "")?.label || "—"}</b></div>
                <div className="vrow"><span>profile</span><b>{preview.profile_revision ? `v${preview.profile_revision}` : "—"}</b></div>
                {preview.allowed ? <span className="badge b-ok">would launch</span> : <p className="verr">{(preview.reasons ?? []).join("\n") || "not allowed"}</p>}
              </div>
            </>
          )}
          <p className="hint">Cadence-managed launches need no files: the registry is read at launch. Preview does not start an agent; it resolves the saved profile, harness, model and permission.</p>
        </>
      );
    }

    return (
      <>
        <div className="scrim" onClick={closeDrawer} />
        <aside ref={drawerRef} className="drawer" role="dialog" aria-modal="true" aria-label={title || "editor"}>
          <div className="dhead">
            <div><div className="slabel">{kicker}</div><h2>{title}</h2></div>
            <Button size="sm" variant="ghost" onClick={closeDrawer}>Close</Button>
          </div>
          <div className="dbody">{body}</div>
        </aside>
      </>
    );
  }

  return (
    <section className="agents-settings" aria-busy={loading || saving}>
      <div className="page-head">
        <div>
          <h1>{t("Agents")}</h1>
          <p className="sub">{t(sub)}</p>
        </div>
        <div className="head-actions">
          <Button disabled={frozen} onClick={() => openNew(tab)}>{tab === "harnesses" ? t("Add harness") : tab === "profiles" ? t("Add profile") : t("Add provider")}</Button>
          {tab === "profiles" && <Button disabled={frozen || dirty || syncBusy} title={dirty ? "Save before sync" : undefined} onClick={() => void sync(null)}>Sync all files</Button>}
          <Button disabled={!saved?.config.profiles.length || dirty} title={dirty ? "Save before preview" : undefined} onClick={() => { if (!saved?.config.profiles.length) return; setDrawer({ kind: "preview", profileId: saved.config.profiles[0]?.id || "", harnessId: saved.config.harnesses[0]?.id || "" }); }}>Preview a launch…</Button>
        </div>
      </div>
      <p className="notice quiet">{t("One registry, versioned with the project. Everything here is created and managed on this page: harnesses, model providers, their models, and the profiles that use them. Catalog rows are observations, not permission to launch.")}</p>
      {readOnly && <p className="notice" role="status">{saved?.read_only || gate === READ_ONLY_REASON ? t("This board is read-only. You can inspect agent settings here.") : t("Sign in using the top bar to edit agent settings.")}</p>}
      {loading && !saved && <p role="status">{t("Loading agent settings…")}</p>}
      {loadError && (
        <div className="notice bad" role="alert">
          <p>{loadError === UI_LOAD ? t(loadError) : loadError}</p>
          <Button onClick={() => void reload()}>{t("Retry")}</Button>
        </div>
      )}
      {saved && (
        <>
          <nav className="tabs" role="tablist" aria-label="agents settings">
            {(["harnesses", "profiles", "models"] as Tab[]).map((id) => (
              <button key={id} className={`tab${tab === id ? " on" : ""}`} type="button" role="tab" aria-selected={tab === id} onClick={() => setTab(id)}>
                {id === "harnesses" ? "Harnesses" : id === "profiles" ? "Profiles" : "Providers & models"}
                <span className="tabn">{id === "harnesses" ? draft.harnesses.length : id === "profiles" ? draft.profiles.length : draft.providers.length}</span>
              </button>
            ))}
          </nav>
          <div className="pane" role="tabpanel">
            {tab === "harnesses" && renderHarnesses()}
            {tab === "profiles" && renderProfiles()}
            {tab === "models" && renderModels()}
          </div>
          {!!issues.length && <div className="notice bad" role="alert">{issues.map((item) => <p key={item}>{item}</p>)}</div>}
          {conflict && (
            <div className="notice" role="alert">
              <p>{saveError || t(UI_CONFLICT)}</p>
              <Button disabled={saving} onClick={() => void reload()}>{t("Reload server copy")}</Button>
            </div>
          )}
          {saveError && !conflict && <p className="notice bad" role="alert">{saveError}</p>}
          <div className="savebar">
            <span className={`badge ${issues.length ? "b-fail" : dirty ? "b-warn" : "plain"}`}>{issues.length ? `${issues.length} problem${issues.length > 1 ? "s" : ""}` : changeCount ? `${changeCount} change${changeCount > 1 ? "s" : ""}` : "no changes"}</span>
            <span className="ref">{readOnly ? "read-only: saving needs an operator session" : `revision ${saved.revision}`}</span>
            <span className="ref" style={{ marginLeft: "auto" }}>{note}</span>
            <Button size="sm" disabled={!dirty || saving} onClick={() => { if (saved) { setDraft(normalize(saved.config)); setSaveError(null); setConflict(false); setNote("Draft reset to the last saved revision."); closeDrawer(); } }}>Discard</Button>
            <Button size="sm" variant="primary" disabled={!dirty || !!issues.length || frozen || conflict} loading={saving} onClick={() => void save()}>Save</Button>
          </div>
        </>
      )}
      {renderDrawer()}
    </section>
  );
}
