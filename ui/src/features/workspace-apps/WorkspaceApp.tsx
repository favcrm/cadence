import { useCallback, useEffect, useRef, useState } from "react";
import type { Agent } from "../../lib/types";
import type { Viewer } from "../projects/work";
import { ApiError } from "../../lib/api";
import { StoredDraftCard } from "./StoredDraftCard";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import {
  IconApps,
  IconProjects,
  IconWiki,
  IconRefresh,
  IconLock,
} from "../../ui/icons";
import { HorizontalStrip } from "./HorizontalStrip";
import { WorkspaceDialog } from "./WorkspaceDialog";
import { NewPost, type NewPostValues, type SelectedSource } from "./NewPost";
import { SourceImport, type SourceImportValues } from "./SourceImport";
import { SourcesPanel } from "./SourcesPanel";
import { plainTitle, runLane, statusText, statusTone } from "./presentation";
import {
  workspaceApps,
  type Installation,
  type AppContext,
  type AppBinding,
  type WorkspaceRun,
  type TextArtifact,
  type AppEffect,
  type Connection,
  type WorkspaceOutbox,
} from "./workspaceApps";
import { retainedRequest, completeRequest } from "./requests";
import "./workspace-apps.css";

type Section =
  | "Board"
  | "Library"
  | "Runs"
  | "Needs you"
  | "Settings"
  | "Schedule"
  | "Sources";
type Snapshot = {
  installation: Installation;
  contexts: AppContext[];
  bindings: AppBinding[];
  connections: Connection[];
  agents: Agent[];
  runs: WorkspaceRun[];
  effects: AppEffect[];
};
const message = (error: unknown) =>
  error instanceof Error
    ? error.message
    : "Could not complete this action. Refresh and try again.";

export default function WorkspaceApp({
  installId,
  viewer,
  onBack,
}: {
  installId: string;
  viewer: Viewer;
  onBack?: () => void;
}) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [section, setSection] = useState<Section>("Board");
  const [contextId, setContextId] = useState("");
  const [selectedRun, setSelectedRun] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [importing, setImporting] = useState(false);
  const [selectedSource, setSelectedSource] = useState<SelectedSource | null>(null);
  const [busy, setBusy] = useState(false);
  const mutationLock = useRef(false);
  const activeRead = useRef<AbortController | null>(null);
  const identity = useRef(installId);
  identity.current = installId;
  const [artifact, setArtifact] = useState<TextArtifact | null>(null);
  const [artifactError, setArtifactError] = useState<string | null>(null);
  const [artifactLoading, setArtifactLoading] = useState(false);
  const [selectedArtifact, setSelectedArtifact] = useState("");
  const [brandName, setBrandName] = useState("");
  const [brandVoice, setBrandVoice] = useState("");
  const [protectedTerms, setProtectedTerms] = useState("");
  const [connectionId, setConnectionId] = useState("");
  const [sourceConnectionId, setSourceConnectionId] = useState("");
  const [outbox, setOutbox] = useState<WorkspaceOutbox | null>(null);
  const [outboxError, setOutboxError] = useState<string | null>(null);
  const [selectedEffectId, setSelectedEffectId] = useState("");
  const [accessDenied, setAccessDenied] = useState(false);
  const clearPrivate = useCallback(() => {
    activeRead.current?.abort();
    activeRead.current = null;
    setLoading(false);
    setBrandName(""); setBrandVoice(""); setProtectedTerms("");
    setContextId(""); setConnectionId("");
    setSourceConnectionId(""); setImporting(false); setSelectedSource(null);
    setData(null); setSelectedRun(null); setSelectedArtifact(""); setArtifact(null);
    setOutbox(null); setSelectedEffectId(""); setCreating(false); setAccessDenied(true);
  }, []);
  const refused = (error: unknown) => error instanceof ApiError && [401, 403].includes(error.status);
  const canWrite = viewer.operator && !viewer.readOnly && !accessDenied;
  const refresh = useCallback(async () => {
    if (!viewer.operator) {
      setLoading(false);
      return;
    }
    if (activeRead.current && !activeRead.current.signal.aborted) return;
    const controller = new AbortController();
    activeRead.current = controller;
    setLoading(true);
    try {
      const [
        installation,
        contexts,
        bindings,
        connections,
        agents,
        runs,
        effects,
      ] = await Promise.all([
        workspaceApps.detail(installId, controller.signal),
        workspaceApps.contexts(installId, controller.signal),
        workspaceApps.bindings(installId, undefined, controller.signal),
        workspaceApps.connections(controller.signal),
        workspaceApps.agents(controller.signal),
        workspaceApps.runs(installId, undefined, controller.signal),
        workspaceApps.effects(installId, undefined, controller.signal),
      ]);
      if (!controller.signal.aborted) {
        setAccessDenied(false);
        setData({
          installation,
          contexts,
          bindings,
          connections,
          agents,
          runs,
          effects,
        });
        setLoadError(null);
      }
    } catch (error) {
      if (!controller.signal.aborted) {
        if (refused(error)) clearPrivate();
        setLoadError(message(error));
      }
    } finally {
      if (activeRead.current === controller) activeRead.current = null;
      if (!controller.signal.aborted) setLoading(false);
    }
  }, [installId, viewer.operator, clearPrivate]);
  useEffect(() => {
    setData(null);
    setBrandName(""); setBrandVoice(""); setProtectedTerms(""); setConnectionId("");
    setSourceConnectionId(""); setImporting(false); setSelectedSource(null);
    setContextId("");
    setSelectedRun(null);
    setSelectedEffectId("");
    setArtifact(null);
    setOutbox(null);
    setLoading(true);
    setActionError(null);
    void refresh();
    const timer = window.setInterval(() => {
      if (!mutationLock.current && document.visibilityState !== "hidden")
        void refresh();
    }, 5000);
    return () => {
      window.clearInterval(timer);
      activeRead.current?.abort();
    };
  }, [refresh]);
  const mutate = async (work: () => Promise<void>) => {
    if (!canWrite || mutationLock.current) return;
    mutationLock.current = true;
    setBusy(true);
    setActionError(null);
    const expectedInstall = installId;
    try {
      await work();
    } catch (error) {
      if (identity.current === expectedInstall) {
        if (refused(error)) clearPrivate();
        setActionError(message(error));
      }
    } finally {
      mutationLock.current = false;
      setBusy(false);
      if (identity.current === expectedInstall) await refresh();
    }
  };
  const runs =
    data?.runs.filter((run) => (run.context_id ?? "") === contextId) ?? [];
  const isSourceRun = (value: WorkspaceRun) => !!value.snapshot.inputs.profile_handle;
  const sourceRuns = runs.filter(isSourceRun);
  const postRuns = runs.filter(value => !isSourceRun(value));
  const effects =
    data?.effects.filter(
      (effect) => (effect.authority.context?.id ?? "") === contextId,
    ) ?? [];
  const run = data?.runs.find((value) => value.id === selectedRun) ?? null;
  const runEffects =
    data?.effects.filter((effect) => effect.authority.run_id === run?.id) ?? [];
  const selectedEffect =
    runEffects.find((effect) => effect.effect_id === selectedEffectId) ??
    runEffects[0] ??
    null;
  const binding = data?.bindings.find(
    (value) =>
      value.context_id === (contextId || null) &&
      value.slot === "publication" &&
      value.state === "configured",
  );
  const sourceBinding = data?.bindings.find(value => value.context_id === (contextId || null) && value.slot === "source" && value.state === "configured");
  const contexts =
    data?.contexts.filter((value) => value.state === "active") ?? [];
  const agents = data?.agents ?? [];
  const workers = agents.filter(
    (agent) =>
      agent.role === "worker" &&
      !agent.dead &&
      !agent.fenced &&
      !agent.inbox &&
      ((agent.provider === "codex" &&
        ["managed", "managed-ws"].includes(agent.endpoint_kind)) ||
        (["claude", "pi"].includes(agent.provider) &&
          agent.endpoint_kind === "managed") ||
        (agent.provider === "fake" && agent.endpoint_kind === "fake")),
  );
  const managers = agents.filter(
    (agent) => agent.role === "pm" && !agent.dead && !agent.fenced,
  );
  const options = (rows: Agent[]) =>
    rows.map((agent) => ({
      value: agent.alias,
      label: agent.alias,
      hint: agent.state_label || agent.state,
      group: agent.group,
    }));
  const workflowOptions = ["instagram", "facebook"]
    .filter((name) =>
      data?.installation.files?.includes(`workflows/${name}.md`),
    )
    .map((value) => ({
      value,
      label: value === "instagram" ? "Instagram caption" : "Facebook post",
    }));
  const approved = data?.installation.approved === true;
  const sourceEnabled = data?.installation.version === "0.3.0" && data.installation.files?.includes("workflows/source-instagram.md");
  useEffect(() => {
    setArtifact(null);
    setArtifactError(null);
    setArtifactLoading(false);
    const artifactId = selectedArtifact || run?.artifacts[0]?.id;
    if (!artifactId || !run?.artifacts.some((value) => value.id === artifactId))
      return;
    const controller = new AbortController();
    setArtifactLoading(true);
    void workspaceApps
      .artifact(artifactId, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setArtifact(value);
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          if (refused(error)) clearPrivate();
          setArtifactError(message(error));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setArtifactLoading(false);
      });
    return () => controller.abort();
  }, [
    run?.id,
    selectedArtifact,
    run?.artifacts.map((value) => value.id).join(","),
  ]);
  useEffect(() => {
    setOutbox(null);
    setOutboxError(null);
    if (
      !selectedEffect ||
      !["done", "reconcile", "closed"].includes(selectedEffect.state)
    )
      return;
    const controller = new AbortController();
    void workspaceApps
      .outbox(selectedEffect.effect_id, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setOutbox(value);
      })
      .catch((error) => {
        if (!controller.signal.aborted) {
          if (refused(error)) clearPrivate();
          setOutboxError(message(error));
        }
      });
    return () => controller.abort();
  }, [selectedEffect?.effect_id, selectedEffect?.state]);
  const openRun = (value: WorkspaceRun) => {
    setSelectedArtifact("");
    setSelectedEffectId("");
    setSelectedRun(value.id);
    setActionError(null);
  };
  const create = (values: NewPostValues) =>
    void mutate(async () => {
      const requestKey = `${installId}.${contextId}.run.${JSON.stringify(values)}`;
      const created = await workspaceApps.createRun({
        install_id: installId,
        workflow: values.workflow,
        inputs: {
          subject: values.title,
          ...(!values.sourceReceiptId ? { source: values.source } : {}),
          writer: values.writer,
          reviewer: values.reviewer,
        },
        request_id: retainedRequest(requestKey),
        owner_pm: values.ownerPm,
        ...(values.sourceReceiptId && values.selectedPostId ? { source_receipt_id: values.sourceReceiptId, selected_post_id: values.selectedPostId } : {}),
        ...(contextId ? { context_id: contextId } : {}),
      });
      completeRequest(requestKey);
      if (identity.current !== installId) return;
      setCreating(false);
      setSelectedSource(null);
      setSelectedRun(created.id);
      setSelectedArtifact("");
    });
  const createSource = (values: SourceImportValues) =>
    void mutate(async () => {
      const requestKey = `${installId}.${contextId}.source.${JSON.stringify(values)}`;
      const created = await workspaceApps.createRun({
        install_id: installId,
        workflow: "source-instagram",
        inputs: { profile_handle: values.profileHandle, writer: values.writer },
        request_id: retainedRequest(requestKey),
        owner_pm: values.ownerPm,
        ...(contextId ? { context_id: contextId } : {}),
      });
      completeRequest(requestKey);
      if (identity.current !== installId) return;
      setImporting(false);
      setSelectedRun(created.id);
    });
  const card = (value: WorkspaceRun) => {
    const released = effects.some(
      (effect) =>
        effect.authority.run_id === value.id && effect.state === "done",
    );
    const state = released ? "done" : value.state;
    return (
      <button
        key={value.id}
        type="button"
        className="wa-post-card"
        data-tone={statusTone(state)}
        onClick={() => openRun(value)}
      >
        <div className="wa-card-meta">
          <span className="wa-status" data-tone={statusTone(state)}>
            {statusText(state)}
          </span>
          <span className="wa-kicker">Text</span>
        </div>
        <strong>
          {plainTitle(value.snapshot.inputs, value.snapshot.workflow.title)}
        </strong>
        <p>
          {value.snapshot.inputs.source?.slice(0, 180) ||
            "Open the stored run to inspect its source and team."}
        </p>
        <span className="wa-kicker">
          {value.steps.filter((step) => step.state === "succeeded").length}/
          {value.steps.length} steps complete
        </span>
      </button>
    );
  };
  const needs = postRuns.filter(
    (value) =>
      ["awaiting_approval", "approved", "succeeded", "failed"].includes(
        value.state,
      ) &&
      (!effects.some(
        (effect) =>
          effect.authority.run_id === value.id && effect.state === "done",
      ) ||
        effects.some(
          (effect) => effect.authority.run_id === value.id && effect.needs_you,
        )),
  );
  const listedRuns =
    section === "Needs you"
      ? needs
      : section === "Library"
        ? postRuns.filter((value) =>
            value.artifacts.some((item) =>
              value.reviews.some(
                (review) =>
                  review.artifact_digest === item.digest &&
                  review.decision === "approve",
              ),
            ),
          )
        : postRuns;
  const lanes = [
    {
      id: "drafting",
      label: "Drafting",
      rows: postRuns.filter((value) => runLane(value) === "drafting"),
    },
    {
      id: "review",
      label: "In review",
      rows: postRuns.filter((value) => runLane(value) === "review"),
    },
    {
      id: "waiting",
      label: "Needs you",
      rows: postRuns.filter(
        (value) =>
          runLane(value) === "waiting" &&
          !effects.some(
            (effect) =>
              effect.authority.run_id === value.id && effect.state === "done",
          ),
      ),
    },
    {
      id: "released",
      label: "Released to Local",
      rows: postRuns.filter((value) =>
        effects.some(
          (effect) =>
            effect.authority.run_id === value.id && effect.state === "done",
        ),
      ),
    },
  ];
  if (!viewer.operator) return <main className="workspace-app" aria-label="Workspace app"><h1>Workspace app</h1><p className="wa-alert">Sign in as the operator to inspect this installation.</p><Button href="/apps">All apps</Button></main>;
  if (data && (data.installation.name !== "social-content" || !["0.2.0", "0.3.0"].includes(data.installation.version) || workflowOptions.length !== 2)) {
    return <main className="workspace-app" aria-label="Workspace app">
      <header className="wa-header"><h1>{data.installation.title || data.installation.name}</h1><Button href="/apps">All apps</Button></header>
      <section className="wa-panel wa-stack">
        <p>{data.installation.summary}</p>
        <p className="wa-muted">This installation has no supported board workspace yet. Use its installed guide and the app commands to inspect supported workflows.</p>
        <p className="wa-kicker">Installation {installId} · version {data.installation.version}</p>
        <pre className="wa-preview">{data.installation.guide}</pre>
      </section>
    </main>;
  }
  return (
    <main className="workspace-app" aria-label="Workspace app">
      <header className="wa-header">
        <div className="wa-heading">
          <span className="wa-mark">
            <IconApps size={22} />
          </span>
          <div>
              <h1>{data?.installation.title || "Workspace app"}</h1>
            <p className="wa-kicker">
              Workspace app · text drafts and Local release
            </p>
          </div>
        </div>
        <div className="wa-row">
          {onBack && <Button onClick={onBack}>All apps</Button>}
          <Button
            icon={<IconRefresh />}
            loading={loading}
            onClick={() => void refresh()}
          >
            Refresh
          </Button>
        </div>
      </header>
      {!canWrite && (
        <p className="wa-alert">
          <IconLock /> Read-only view. A verified operator can configure the
          app, approve runs and release text.
        </p>
      )}
      {loadError && (
        <p className="wa-alert" data-tone="fail" role="alert">
          {loadError}
          {data
            ? " Showing the last loaded state. Decisions still require current server authority."
            : ""}
        </p>
      )}
      {actionError && !creating && !run && (
        <p className="wa-alert" data-tone="fail" role="alert">
          {actionError}
        </p>
      )}
      {loading && !data && (
        <p className="wa-empty" role="status">
          Loading this workspace app…
        </p>
      )}
      {data && (
        <>
          <div className="wa-toolbar">
            <div className="wa-context">
              <Select
                value={contextId}
                onChange={(value) => {
                  setContextId(value);
                  setConnectionId("");
                  setSourceConnectionId("");
                  setSelectedSource(null);
                  setSelectedRun(null);
                  setSelectedArtifact("");
                  setActionError(null);
                }}
                options={[
                  { value: "", label: "No brand context" },
                  ...contexts.map((value) => ({
                    value: value.id,
                    label: value.config.label,
                  })),
                ]}
                aria-label="Optional brand context"
                disabled={busy}
                full
              />
            </div>
            <Button
              variant="primary"
              onClick={() => {
                setActionError(null);
                setCreating(true);
              }}
              disabled={
                !canWrite ||
                !approved ||
                !binding ||
                busy ||
                !workflowOptions.length
              }
            >
              New post
            </Button>
          </div>
          {!approved && (
            <div className="wa-alert">
              Approve this app’s current text workflow bundle before starting a
              post.{" "}
              <Button
                size="sm"
                onClick={() =>
                  void mutate(async () => {
                    await workspaceApps.approveInstall(
                      installId,
                      data.installation.digest,
                    );
                  })
                }
                disabled={!canWrite}
                loading={busy}
              >
                Approve app
              </Button>
            </div>
          )}
          {approved && !binding && (
            <div className="wa-alert">
              Choose and save a Local destination in Settings before creating a
              run. The destination is frozen into its plan.{" "}
              <Button size="sm" onClick={() => setSection("Settings")}>
                Set destination
              </Button>
            </div>
          )}
          <nav className="wa-tabs" aria-label="App sections">
            {(
              [
                "Board",
                "Library",
                ...(sourceEnabled ? ["Sources"] : []),
                "Runs",
                "Needs you",
                "Schedule",
                "Settings",
              ] as Section[]
            ).map((value) => (
              <button
                key={value}
                type="button"
                className="wa-tab"
                aria-current={section === value ? "page" : undefined}
                onClick={() => setSection(value)}
              >
                {value === "Board" ? (
                  <IconProjects />
                ) : value === "Library" ? (
                  <IconWiki />
                ) : null}
                {value}
                {value === "Needs you" && needs.length ? (
                  <span className="wa-status">{needs.length}</span>
                ) : null}
              </button>
            ))}
          </nav>
          <div className="wa-content">
            {section === "Board" && (
              <HorizontalStrip label="Post board" className="wa-board">
                {lanes.map((lane) => (
                  <section key={lane.id} className="wa-lane">
                    <header className="wa-lane-head">
                      <h3>{lane.label}</h3>
                      <span className="wa-status">{lane.rows.length}</span>
                    </header>
                    <div className="wa-lane-body">
                      {lane.rows.length ? (
                        lane.rows.map(card)
                      ) : (
                        <p className="wa-empty">
                          No posts in {lane.label.toLowerCase()}.
                        </p>
                      )}
                    </div>
                  </section>
                ))}
              </HorizontalStrip>
            )}
            {(section === "Runs" ||
              section === "Needs you" ||
              section === "Library") && (
              <div className="wa-grid">
                {section === "Library" ? listedRuns.map(value => <StoredDraftCard key={value.id} run={value} onOpen={() => openRun(value)} onDenied={clearPrivate} />) : listedRuns.map(card)}
                {!listedRuns.length && (
                  <div className="wa-panel wa-empty">
                    {section === "Library"
                      ? "No accepted text yet. Your writer and reviewer’s completed posts will appear here."
                      : section === "Needs you"
                        ? "Nothing needs your decision right now."
                        : "No posts yet. Choose your team and create a post from pasted source facts."}
                  </div>
                )}
              </div>
            )}
            {section === "Schedule" && (
              <section className="wa-panel wa-stack">
                <h2>Schedule</h2>
                <p className="wa-muted">
                  Scheduling isn’t available in this version. Posts have no
                  planned dates yet. Your accepted text stays in Library until
                  you explicitly release it to Local.
                </p>
                <p className="wa-kicker">
                  Instagram and Facebook publishing and generated images
                  aren’t connected.
                </p>
              </section>
            )}
            {section === "Sources" && sourceEnabled && (
              <SourcesPanel
                runs={sourceRuns}
                canWrite={canWrite && approved && !!sourceBinding}
                canCreate={canWrite && approved && !!binding}
                onImport={() => { setActionError(null); setImporting(true); }}
                onOpenRun={openRun}
                onPick={source => { setSelectedSource(source); setCreating(true); setActionError(null); }}
                onDenied={clearPrivate}
              />
            )}
            {section === "Settings" && (
              <div className="wa-stack">
                <section className="wa-panel wa-stack">
                  <h2>Local destination</h2>
                  <p className="wa-muted">
                    Save the actual destination for{" "}
                    {contextId
                      ? contexts.find((value) => value.id === contextId)?.config
                          .label
                      : "posts without a brand context"}
                    . This configures future plans; it doesn’t release a post.
                  </p>
                  {binding && (
                    <p className="wa-kicker">
                      Configured · revision {binding.revision} ·{" "}
                      {binding.config.account}
                    </p>
                  )}
                  <div className="wa-row">
                    <div className="wa-context">
                      <Select
                        value={
                          connectionId || binding?.config.connection_id || ""
                        }
                        onChange={setConnectionId}
                        options={data.connections
                          .filter((value) => value.provider === "local")
                          .map((value) => ({
                            value: value.id,
                            label: value.account || "Local",
                            hint: value.id,
                          }))}
                        placeholder="Choose a Local connection"
                        aria-label="Local destination"
                        disabled={!canWrite || busy}
                        full
                      />
                    </div>
                    <Button
                      disabled={
                        !canWrite ||
                        !(connectionId || binding?.config.connection_id) ||
                        (binding !== undefined &&
                          (connectionId || binding.config.connection_id) ===
                            binding.config.connection_id)
                      }
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          const selected =
                            connectionId || binding?.config.connection_id || "";
                          if (binding)
                            await workspaceApps.updateBinding(
                              installId,
                              binding.id,
                              {
                                expected_revision: binding.revision,
                                connection_id: selected,
                              },
                            );
                          else
                            await workspaceApps.createBinding(installId, {
                              slot: "publication",
                              connection_id: selected,
                              request_id: retainedRequest(
                                `${installId}.${contextId}.binding.${selected}`,
                              ),
                              ...(contextId ? { context_id: contextId } : {}),
                            });
                        })
                      }
                    >
                      Save destination
                    </Button>
                  </div>
                  {!data.connections.some(
                    (value) => value.provider === "local",
                  ) && (
                    <p className="wa-alert">
                      No Local connection is registered. Configure a Local
                      destination before creating a post.
                    </p>
                  )}
                </section>
                {sourceEnabled && <section className="wa-panel wa-stack">
                  <h2>Instagram source connection</h2>
                  <p className="wa-muted">Bind this installation and brand context to a company-scoped AgenticOS device credential with provider.read access. A source run freezes this binding before any provider read.</p>
                  {sourceBinding && <p className="wa-kicker">Configured · revision {sourceBinding.revision} · {sourceBinding.config.account}</p>}
                  <div className="wa-row">
                    <div className="wa-context">
                      <Select
                        value={sourceConnectionId || sourceBinding?.config.connection_id || ""}
                        onChange={setSourceConnectionId}
                        options={data.connections.filter(value => value.provider === "agenticos_external" && value.descriptor?.action_mappings?.some(mapping => mapping.capability === "social.read" && mapping.action === "list_posts" && mapping.effect === "read") && value.status?.manifest_status === "matched" && value.status.custody_available).map(value => ({ value: value.id, label: value.account, hint: value.id }))}
                        placeholder="Choose AgenticOS connection"
                        aria-label="Instagram source connection"
                        disabled={!canWrite || busy}
                        full
                      />
                    </div>
                    <Button
                      disabled={!canWrite || busy || !(sourceConnectionId || sourceBinding?.config.connection_id) || (sourceBinding !== undefined && (sourceConnectionId || sourceBinding.config.connection_id) === sourceBinding.config.connection_id)}
                      loading={busy}
                      onClick={() => void mutate(async () => {
                        const selected = sourceConnectionId || sourceBinding?.config.connection_id || "";
                        if (sourceBinding) await workspaceApps.updateBinding(installId, sourceBinding.id, { expected_revision: sourceBinding.revision, connection_id: selected });
                        else await workspaceApps.createBinding(installId, { slot: "source", connection_id: selected, request_id: retainedRequest(`${installId}.${contextId}.source-binding.${selected}`), ...(contextId ? { context_id: contextId } : {}) });
                      })}
                    >Save source connection</Button>
                  </div>
                  {!data.connections.some(value => value.provider === "agenticos_external") && <p className="wa-alert">No AgenticOS external connection is enrolled. Ask the operator to connect a company-scoped provider.read device credential first.</p>}
                </section>}
                <section className="wa-panel wa-stack">
                  <h2>Brand contexts</h2>
                  <p className="wa-muted">
                    A brand is optional app context. A project isn’t required.
                  </p>
                  <form
                    className="wa-stack"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (!brandName.trim()) return;
                      if (
                        /[\r\n\t]/.test(brandName + brandVoice + protectedTerms)
                      ) {
                        setActionError(
                          "Brand name, voice and protected terms must be single-line text without tabs.",
                        );
                        return;
                      }
                      void mutate(async () => {
                        const context = await workspaceApps.createContext(
                          installId,
                          {
                            label: brandName.trim(),
                            input_defaults: {
                              ...(brandVoice.trim()
                                ? { brand_voice: brandVoice.trim() }
                                : {}),
                              ...(protectedTerms.trim()
                                ? { protected_terms: protectedTerms.trim() }
                                : {}),
                            },
                            request_id: retainedRequest(
                              JSON.stringify({
                                installId,
                                brandName: brandName.trim(),
                                brandVoice: brandVoice.trim(),
                                protectedTerms: protectedTerms.trim(),
                              }),
                            ),
                          },
                        );
                        if (identity.current === installId) {
                          setContextId(context.id);
                          setBrandName("");
                          setBrandVoice("");
                          setProtectedTerms("");
                          setConnectionId("");
                          setSourceConnectionId("");
                        }
                      });
                    }}
                  >
                    <div className="wa-field wa-context">
                      <label htmlFor="wa-brand-name">New brand name</label>
                      <input
                        id="wa-brand-name"
                        name="brand"
                        className="wa-input"
                        value={brandName}
                        onChange={(event) => setBrandName(event.target.value)}
                        maxLength={120}
                        required
                        disabled={!canWrite || busy}
                      />
                    </div>
                    <div className="wa-fields">
                      <div className="wa-field">
                        <label htmlFor="wa-brand-voice">
                          Brand voice (optional)
                        </label>
                        <input
                          id="wa-brand-voice"
                          name="brand_voice"
                          className="wa-input"
                          value={brandVoice}
                          onChange={(event) =>
                            setBrandVoice(event.target.value)
                          }
                          maxLength={2000}
                          disabled={!canWrite || busy}
                        />
                      </div>
                      <div className="wa-field">
                        <label htmlFor="wa-protected-terms">
                          Protected terms (optional)
                        </label>
                        <input
                          id="wa-protected-terms"
                          name="protected_terms"
                          className="wa-input"
                          value={protectedTerms}
                          onChange={(event) =>
                            setProtectedTerms(event.target.value)
                          }
                          maxLength={2000}
                          disabled={!canWrite || busy}
                        />
                      </div>
                    </div>
                    <p className="wa-kicker">
                      Use one line for each default. These defaults are frozen
                      into new plans for this brand.
                    </p>
                    <Button type="submit" disabled={!canWrite} loading={busy}>
                      Add brand
                    </Button>
                  </form>
                </section>
                <section className="wa-panel wa-stack">
                  <h2>Available in this version</h2>
                  <p className="wa-muted">
                    {sourceEnabled ? "Retained public Instagram source or pasted facts → writer → independent review → explicit Local release. Image generation, external publishing and scheduling are unavailable." : "Pasted text → actual writer → independent review → explicit Local release. Remote sources, image generation, external publishing and scheduling are unavailable."}
                  </p>
                  <details className="wa-details">
                    <summary>Installation details</summary>
                    <p className="wa-digest">
                      {installId}
                      <br />
                      {data.installation.digest}
                    </p>
                  </details>
                </section>
              </div>
            )}
          </div>
        </>
      )}
      {creating && (
        <NewPost
          workflows={workflowOptions}
          workers={options(workers)}
          managers={options(managers)}
          busy={busy}
          error={actionError}
          selectedSource={selectedSource}
          onClose={() => {
            if (!busy) { setCreating(false); setSelectedSource(null); }
          }}
          onCreate={create}
        />
      )}
      {importing && sourceEnabled && (
        <SourceImport
          installId={installId}
          contextId={contextId || undefined}
          bindingDigest={sourceBinding?.digest || ""}
          workers={options(workers)}
          managers={options(managers)}
          busy={busy}
          error={actionError}
          onDenied={clearPrivate}
          onClose={() => { if (!busy) setImporting(false); }}
          onCreate={createSource}
        />
      )}
      {run && isSourceRun(run) && (
        <WorkspaceDialog title={`Read @${run.snapshot.inputs.profile_handle}`} onClose={() => { if (!busy) setSelectedRun(null); }}>
          <div className="wa-stack">
            {actionError && <p className="wa-alert" data-tone="fail" role="alert">{actionError}</p>}
            <div className="wa-row"><span className="wa-status" data-tone={statusTone(run.state)}>{statusText(run.state)}</span><span className="wa-kicker">{run.id}</span></div>
            <p className="wa-muted">This run reads public posts from the frozen @{run.snapshot.inputs.profile_handle} profile. It cannot publish or generate an image.</p>
            {run.snapshot.quotes?.source?.currency === "USD" && Number.isSafeInteger(run.snapshot.quotes.source.total_price_micros) ? <p className="wa-alert">Frozen provider charge: <strong>USD {(run.snapshot.quotes.source.total_price_micros / 1_000_000).toFixed(6)}</strong> for one read. Binding {run.snapshot.capabilities?.source?.digest || "unavailable"}. A changed quote or binding stops dispatch.</p> : <p className="wa-alert" data-tone="fail">This source plan has no verified provider quote. It cannot be approved.</p>}
            <details className="wa-details"><summary>Inspect the exact source plan</summary><pre>{JSON.stringify(run.snapshot, null, 2)}</pre><p className="wa-digest">{run.snapshot_digest}</p></details>
            {run.state === "awaiting_approval" && <Button variant="primary" disabled={!canWrite || !run.snapshot.quotes?.source || !run.snapshot.capabilities?.source} loading={busy} onClick={() => void mutate(async () => { await workspaceApps.approveRun(run.id, run.snapshot_digest); })}>Approve source plan</Button>}
            {run.state === "approved" && <Button variant="primary" disabled={!canWrite || !run.snapshot.quotes?.source || !run.snapshot.capabilities?.source} loading={busy} onClick={() => void mutate(async () => { await workspaceApps.dispatchRun(run.id); })}>Start source reader</Button>}
            {run.state === "running" && <p className="wa-muted" role="status">Waiting for the assigned reader and provider receipt. Refresh Sources to see retained posts.</p>}
            {run.state === "failed" && <p className="wa-alert" data-tone="fail">The source run failed. No posts will be fabricated. Inspect its exact plan and stored run for details.</p>}
            {run.state === "succeeded" && <p className="wa-muted">The provider receipt is available in Sources. Choose one post there to start a caption.</p>}
          </div>
        </WorkspaceDialog>
      )}
      {run && !isSourceRun(run) && (
        <WorkspaceDialog
          title={plainTitle(run.snapshot.inputs, run.snapshot.workflow.title)}
          onClose={() => {
            if (!busy) setSelectedRun(null);
          }}
        >
          <div className="wa-stack">
            {actionError && (
              <p className="wa-alert" data-tone="fail" role="alert">
                {actionError}
              </p>
            )}
            <div className="wa-row">
              <span className="wa-status" data-tone={statusTone(run.state)}>
                {statusText(run.state)}
              </span>
              <span className="wa-kicker">{run.id}</span>
            </div>
            <p className="wa-muted">{run.snapshot.inputs.source}</p>
            <section className="wa-stack">
              <h3>Frozen plan</h3>
              {run.snapshot.workflow.steps.map((step) => (
                <div key={step.id} className="wa-step">
                  <span>
                    {step.kind === "review_text"
                      ? "Independent review"
                      : "Write text"}{" "}
                    · {step.assignee}
                  </span>
                  <span className="wa-status">
                    {statusText(
                      run.steps.find((value) => value.step_id === step.id)
                        ?.state || "pending",
                    )}
                  </span>
                </div>
              ))}
              <p className="wa-kicker">
                Responsible PM: {run.snapshot.owner_pm}. Destination and brand
                context are frozen into this plan.
              </p>
              <details className="wa-details">
                <summary>Inspect the exact plan</summary>
                <pre>{JSON.stringify(run.snapshot, null, 2)}</pre>
                <p className="wa-digest">{run.snapshot_digest}</p>
              </details>
              {run.state === "awaiting_approval" && (
                <Button
                  variant="primary"
                  disabled={!canWrite}
                  loading={busy}
                  onClick={() =>
                    void mutate(async () => {
                      await workspaceApps.approveRun(
                        run.id,
                        run.snapshot_digest,
                      );
                    })
                  }
                >
                  Approve this plan
                </Button>
              )}
              {run.state === "approved" && (
                <Button
                  variant="primary"
                  disabled={!canWrite}
                  loading={busy}
                  onClick={() =>
                    void mutate(async () => {
                      await workspaceApps.dispatchRun(run.id);
                    })
                  }
                >
                  Start writer and reviewer
                </Button>
              )}
            </section>
            {run.reviews.map((review, index) => (
              <div key={`${review.step_id}-${index}`} className="wa-alert">
                <strong>
                  {review.decision === "approve"
                    ? "Reviewer accepted"
                    : "Reviewer requested changes"}{" "}
                  · {review.reviewer}
                </strong>
                <p>{review.rationale}</p>
                <p className="wa-digest">{review.artifact_digest}</p>
              </div>
            ))}
            {run.artifacts.length > 1 && (
              <Select
                value={selectedArtifact || run.artifacts[0].id}
                onChange={setSelectedArtifact}
                options={run.artifacts.map((value) => ({
                  value: value.id,
                  label: `${value.step_id} · ${value.media_type}`,
                }))}
                aria-label="Stored artifact"
                full
              />
            )}
            {artifactLoading && (
              <p className="wa-muted" role="status">
                Loading stored text…
              </p>
            )}
            {artifactError && (
              <p className="wa-alert" data-tone="fail" role="alert">
                {artifactError}
              </p>
            )}
            {artifact && (
              <section className="wa-stack">
                <h3>Stored text</h3>
                <pre className="wa-preview">{artifact.text}</pre>
                <p className="wa-digest">{artifact.digest}</p>
                {run.state === "succeeded" &&
                  run.reviews.some(
                    (review) =>
                      review.artifact_digest === artifact.digest &&
                      review.decision === "approve",
                  ) && (
                    <Button
                      disabled={!canWrite}
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          const result = await workspaceApps.stageEffect(
                            run.id,
                            {
                              artifact_id: artifact.id,
                              slot: "publication",
                              title: plainTitle(
                                run.snapshot.inputs,
                                run.snapshot.workflow.title,
                              ),
                              request_id: retainedRequest(
                                `${installId}.stage.${run.id}.${artifact.id}`,
                              ),
                            },
                          );
                          if (identity.current === installId)
                            setSelectedEffectId(result.effect_id);
                        })
                      }
                    >
                      Prepare Local release
                    </Button>
                  )}
              </section>
            )}
            {runEffects.length > 1 && (
              <Select
                value={selectedEffect?.effect_id || ""}
                onChange={setSelectedEffectId}
                options={runEffects.map((value) => ({
                  value: value.effect_id,
                  label: `${statusText(value.state)} · ${value.effect_id}`,
                }))}
                aria-label="Release receipt"
                full
              />
            )}
            {selectedEffect && (
              <section className="wa-stack">
                <h3>Local release</h3>
                <span
                  className="wa-status"
                  data-tone={statusTone(selectedEffect.state)}
                >
                  {statusText(selectedEffect.state)}
                </span>
                <p className="wa-muted">
                  Inspect the server’s exact release receipt before deciding.
                  This writes only to the configured Local destination.
                </p>
                {selectedEffect.record.preview && (
                  <pre className="wa-preview">
                    {selectedEffect.record.preview}
                  </pre>
                )}
                <p className="wa-muted">
                  Destination: {selectedEffect.record.account || "Local"}.{" "}
                  {selectedEffect.record.outcome?.verified === true
                    ? "The retained verification confirmed this item."
                    : selectedEffect.record.outcome?.verified === false
                      ? "The retained verification did not confirm this item. Check the outcome."
                      : "No confirmed verification is recorded yet."}
                </p>
                <details className="wa-details">
                  <summary>Exact release receipt</summary>
                  <pre>{JSON.stringify(selectedEffect.record, null, 2)}</pre>
                  <p className="wa-digest">{selectedEffect.digest}</p>
                </details>
                {selectedEffect.state === "waiting" && (
                  <div className="wa-row">
                    <Button
                      variant="primary"
                      disabled={!canWrite}
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          await workspaceApps.decideEffect(
                            selectedEffect.effect_id,
                            {
                              digest: selectedEffect.digest,
                              decision: "accept",
                            },
                          );
                        })
                      }
                    >
                      Release this text to Local
                    </Button>
                    <Button
                      disabled={!canWrite}
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          await workspaceApps.decideEffect(
                            selectedEffect.effect_id,
                            {
                              digest: selectedEffect.digest,
                              decision: "decline",
                            },
                          );
                        })
                      }
                    >
                      Close without releasing
                    </Button>
                  </div>
                )}
                {selectedEffect.state === "reconcile" && (
                  <>
                    <p className="wa-alert">
                      The completion is uncertain. Inspect the retained receipt
                      and destination before closing this case. Cadence will not
                      replay the write.
                    </p>
                    <Button
                      disabled={!canWrite}
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          await workspaceApps.resolveEffect(
                            selectedEffect.effect_id,
                            {
                              digest: selectedEffect.digest,
                              resolution: "close",
                            },
                          );
                        })
                      }
                    >
                      Close after checking outcome
                    </Button>
                  </>
                )}
                {["done", "failed"].includes(selectedEffect.state) &&
                  selectedEffect.needs_you && (
                    <Button
                      disabled={!canWrite}
                      loading={busy}
                      onClick={() =>
                        void mutate(async () => {
                          await workspaceApps.resolveEffect(
                            selectedEffect.effect_id,
                            {
                              digest: selectedEffect.digest,
                              resolution: "acknowledge",
                            },
                          );
                        })
                      }
                    >
                      Acknowledge this outcome
                    </Button>
                  )}
                {outboxError && (
                  <p className="wa-alert" data-tone="fail" role="alert">
                    Could not read the Local outbox: {outboxError}
                  </p>
                )}
                {outbox !== null && (
                  <div className="wa-stack">
                    <h3>Actual Local item</h3>
                    <p className="wa-muted">
                      {outbox.item.title} · {outbox.item.published_at}
                    </p>
                    {outbox.item.post && (
                      <pre className="wa-preview">{outbox.item.post}</pre>
                    )}
                    <details className="wa-details">
                      <summary>Persisted outbox receipt</summary>
                      <pre>{JSON.stringify(outbox, null, 2)}</pre>
                    </details>
                    <p className="wa-kicker">
                      Persisted item and retained verification; this view does
                      not perform a new provider verification.
                    </p>
                  </div>
                )}
              </section>
            )}
          </div>
        </WorkspaceDialog>
      )}
    </main>
  );
}
