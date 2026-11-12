/** CAD-1817 / AOS-378: identity chrome for the pane's admitted actor.
 *  Native Master panes retain the workspace coordinator; hosted panes use
 *  their conversation-bound personal/workspace projection. Every heading,
 *  sender, composer and About panel reads that actor, never a global
 *  translation, Home's cached profile or another user's identity.
 *
 *  Editing stays inside existing authority: a personal avatar change is
 *  an ordinary explicit chat request (the model records `profile.look`
 *  through the current-turn ACK), so the picker never claims the change
 *  was saved; the workspace identity is the operator's proven
 *  `POST /api/master/identity`, and the saved labels come only from the
 *  response identity plus `applies`. */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { sessionKey } from "../../lib/sessionKey";
import { privateSessionGeneration } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { MasterState } from "../../lib/types";
import type { ResourceState } from "../../lib/cache";
import Button from "../../ui/Button";
import { IconClose, IconInfo } from "../../ui/icons";
import AgentAvatar from "../agents/AgentAvatar";
import {
  AVATAR_LOOKS,
  decodeIdentity,
  displayName,
  identityLook,
  messageLabel,
  messagePlaceholder,
  mutatePersonalProfile,
  readConversationIdentity,
  readPersonalBootstrap,
  readPersonalIdentity,
  scopeDescriptor,
  WORKSPACE_ALIAS,
  type AssistantScope,
  type AvatarLook,
  type IdentityRead,
  type ConversationIdentityRead,
  type IdentityRecord,
  type PersonalProfileMutation,
  type PersonalProfileOutcome,
} from "./identity";
import "./assistant-identity.css";

export type PersonalProfileSaveChange = {
  expected_revision: number;
  name?: string | null;
  look?: AvatarLook;
};

export type PersonalProfileSaveResult =
  | { ok: true; identity: IdentityRecord }
  | { ok: false; unsupported?: true; conflict?: true; error: string };

export function postPersonalProfile(req: PersonalProfileMutation): Promise<PersonalProfileOutcome> {
  return mutatePersonalProfile((body) => api.memberAssistProfileUpdate(body), req);
}

export function toPersonalUpdateMutation(change: PersonalProfileSaveChange): PersonalProfileMutation {
  return {
    expected_revision: change.expected_revision,
    action: "update",
    ...(change.name !== undefined ? { name: change.name ?? "" } : {}),
    ...(change.look !== undefined ? { look: change.look } : {}),
  };
}

export function toPersonalCompleteMutation(input: {
  expected_revision: number;
  name?: string;
  look: AvatarLook;
}): PersonalProfileMutation {
  return {
    expected_revision: input.expected_revision,
    action: "complete",
    ...(input.name !== undefined ? { name: input.name } : {}),
    look: input.look,
  };
}

/** Personal identity controls. Hosted boards carry `saveProfile` (`POST /api/member-assistant/profile`);
 *  native boards (or boards predating AOS-570) use `requestLook` chat turns.
 *  `pendingLook` never becomes the displayed identity before a committed read. */
export interface PersonalLookRequest {
  requestLook: (look: AvatarLook) => Promise<{ ok: boolean; error?: string }>;
  pendingLook: AvatarLook | null;
  saveProfile?: (change: PersonalProfileSaveChange) => Promise<PersonalProfileSaveResult>;
}

export type WorkspaceIdentityChange = {
  expected_revision: string;
  name?: string | null;
  tone?: string | null;
  look?: AvatarLook;
};

export type WorkspaceSaveResult =
  | { ok: true; identity: IdentityRecord }
  /** Failed responses do not prove rollback across files and params. Read
   *  persisted state before another attempt; never retry automatically. */
  | { ok: false; error: string };

export interface AssistantActorValue {
  scope: AssistantScope | null;
  /** Avatar fallback key only; it never chooses the actor's profile or scope. */
  alias: string;
  read: IdentityRead;
  /** The model this pane's own conversation reports, for About. */
  model?: string;
  personal: PersonalLookRequest | null;
  workspace: {
    canEdit: boolean;
    save: (change: WorkspaceIdentityChange) => Promise<WorkspaceSaveResult>;
  } | null;
}

/** Outside a provider (e.g. a human thread's bubbles) the historical
 *  generic role label stands; nothing claims a personal name there. */
const FALLBACK_ACTOR: AssistantActorValue = {
  scope: null,
  alias: "",
  read: { state: "unavailable" },
  personal: null,
  workspace: null,
};

const AssistantActorContext = createContext<AssistantActorValue>(FALLBACK_ACTOR);

export function AssistantActorProvider({ actor, children }: { actor: AssistantActorValue; children: ReactNode }) {
  return <AssistantActorContext.Provider value={actor}>{children}</AssistantActorContext.Provider>;
}

export function useAssistantActor(): AssistantActorValue {
  return useContext(AssistantActorContext);
}

/** The selected actor's committed identity. Native keeps its coordinator;
 *  hosted panes read their own conversation projection, never Home's cache.
 *  Personal avatar requests use the pane's existing send path. Only a proven
 *  owner viewing the workspace actor gets the guarded workspace editor. */
export function useConversationActor(
  master: ResourceState<MasterState>,
  canEdit: boolean,
  conversation: string | null,
  sendPersonalRequest: ((text: string) => Promise<void>) | null,
  canEditPersonal: boolean = sendPersonalRequest !== null,
  committedPersonal: IdentityRecord | null = null,
): AssistantActorValue {
  const hosted = master.data !== null && Object.hasOwn(master.data, "actor_identity");
  const source = hosted && conversation !== null ? resources.masterConversation(conversation) : resources.masterState;
  const current = useQuery(source);
  const view: ConversationIdentityRead = current.status === "failed" || current.status === "stale"
    ? { scope: null, read: { state: "unavailable" } }
    : current.data !== null
      ? readConversationIdentity(current.data, conversation)
      : { scope: null, read: { state: "loading" } };
  const live = view.read;
  const model = view.scope === null ? undefined : current.data?.model_label ?? current.data?.model;
  const [saved, setSaved] = useState<{ identity: IdentityRecord; before: string; scope: string } | null>(null);
  const [pending, setPending] = useState<{ look: AvatarLook; scope: string } | null>(null);
  const scopeKey = `${sessionKey() ?? ""}:${privateSessionGeneration()}:${conversation ?? ""}:${view.scope ?? ""}`;
  const scoped = saved !== null && saved.scope === scopeKey ? saved : null;
  const liveCommitted = scoped !== null && live.state === "ready" && String(live.identity.revision) !== scoped.before;
  const baseRead: IdentityRead = scoped !== null && live.state === "ready" && !liveCommitted
    ? { state: "ready", identity: scoped.identity }
    : live;
  const read: IdentityRead =
    view.scope === "personal" &&
    committedPersonal !== null &&
    baseRead.state === "ready" &&
    typeof committedPersonal.revision === "number" &&
    typeof baseRead.identity.revision === "number" &&
    committedPersonal.revision > baseRead.identity.revision
      ? { state: "ready", identity: committedPersonal }
      : baseRead;

  useEffect(() => {
    if (liveCommitted) setSaved(null);
  }, [liveCommitted]);
  useEffect(() => {
    if (pending?.scope === scopeKey && read.state === "ready" && read.identity.look === pending.look) setPending(null);
  }, [pending, scopeKey, read]);
  useEffect(() => {
    if (!hosted || conversation === null) return;
    const timer = window.setInterval(() => { void source.refresh(); }, 6000);
    return () => window.clearInterval(timer);
  }, [hosted, conversation, source]);

  const requestLook = useCallback(async (look: AvatarLook) => {
    if (view.scope !== "personal" || read.state !== "ready" || sendPersonalRequest === null) {
      return { ok: false, error: "The saved assistant identity could not be read." };
    }
    if (read.identity.look === look) return { ok: true };
    try {
      await sendPersonalRequest(`Use the ${look} avatar for yourself.`);
      setPending({ look, scope: scopeKey });
      void source.refresh();
      return { ok: true };
    } catch (error) {
      return { ok: false, error: error instanceof Error ? error.message : String(error) };
    }
  }, [view.scope, read, sendPersonalRequest, scopeKey, source]);

  const saveProfile = useCallback(
    async (change: PersonalProfileSaveChange): Promise<PersonalProfileSaveResult> => {
      if (!hosted || view.scope !== "personal" || !canEditPersonal || read.state !== "ready") {
        return { ok: false, error: "The saved assistant identity could not be read." };
      }
      const before = String(change.expected_revision);
      const outcome = await postPersonalProfile(toPersonalUpdateMutation(change));
      void source.refresh();
      if (source !== resources.masterState) void resources.masterState.refresh();
      if (!outcome.ok) return outcome;
      setSaved({ identity: outcome.identity, before, scope: scopeKey });
      return { ok: true, identity: outcome.identity };
    },
    [hosted, view.scope, canEditPersonal, read, source, scopeKey],
  );

  const save = useCallback(
    async (change: WorkspaceIdentityChange): Promise<WorkspaceSaveResult> => {
      if (view.scope !== "workspace" || !canEdit || read.state !== "ready") {
        return { ok: false, error: "The saved workspace identity could not be read." };
      }
      const before = change.expected_revision;
      try {
        const result = await api.masterIdentityUpdate({
          expected_revision: before,
          ...(change.name !== undefined ? { name: change.name ?? "" } : {}),
          ...(change.tone !== undefined ? { tone: change.tone ?? "" } : {}),
          ...(change.look !== undefined ? { look: change.look } : {}),
        });
        void source.refresh();
        if (source !== resources.masterState) void resources.masterState.refresh();
        try {
          if (result.applies !== "next_turn") throw new Error("The saved workspace identity response was not recognized.");
          const identity = decodeIdentity(result.identity, "workspace");
          setSaved({ identity, before, scope: scopeKey });
          return { ok: true, identity };
        } catch (error) {
          return { ok: false, error: String(error) };
        }
      } catch (error) {
        // A failed response cannot prove rollback. Reconcile, never auto-replay.
        void source.refresh();
        return { ok: false, error: error instanceof ApiError ? error.message : String(error) };
      }
    },
    [view.scope, canEdit, read, source, scopeKey],
  );

  return useMemo<AssistantActorValue>(() => ({
    scope: view.scope,
    alias: view.scope === "workspace" ? WORKSPACE_ALIAS : view.scope === "personal" && read.state === "ready" ? (current.data?.alias ?? "assistant") : "",
    read,
    ...(model ? { model } : {}),
    personal: view.scope === "personal" && read.state === "ready" && (sendPersonalRequest !== null || (hosted && canEditPersonal))
      ? {
          requestLook,
          pendingLook: pending?.scope === scopeKey ? pending.look : null,
          ...(hosted && canEditPersonal ? { saveProfile } : {}),
        }
      : null,
    workspace: view.scope === "workspace" ? { canEdit, save } : null,
  }), [view.scope, current.data?.alias, read, model, sendPersonalRequest, hosted, canEditPersonal, requestLook, saveProfile, pending, scopeKey, canEdit, save]);
}

/** The actor's real avatar (the canonical AgentAvatar artwork). While the
 *  binding alias is still unknown the face is a neutral glyph rather than
 *  a guessed look. */
export function AssistantFace({
  size = 30,
  active = false,
}: {
  size?: number;
  active?: boolean;
}) {
  const actor = useAssistantActor();
  if (actor.alias === "") {
    return (
      <span className="assistant-face assistant-face-empty" style={{ width: size, height: size }} aria-hidden>
        ✳
      </span>
    );
  }
  return (
    <span className="assistant-face">
      <AgentAvatar slug={identityLook(actor.read, actor.alias)} size={size} active={active} still={!active} />
    </span>
  );
}

/** Composer accessible label for the bound actor. */
export function useAssistantComposerLabels(): { placeholder: string; ariaLabel: string } {
  const actor = useAssistantActor();
  const t = useTranslate();
  return useMemo(
    () => ({
      placeholder: messagePlaceholder(actor.read, actor.scope, t),
      ariaLabel: messageLabel(actor.read, actor.scope, t),
    }),
    [actor.read, actor.scope, t],
  );
}

const LOOK_LABEL: Record<AvatarLook, string> = {
  builder: "Builder",
  guard: "Guard",
  bookkeeper: "Bookkeeper",
  content: "Content",
  designer: "Designer",
  reception: "Reception",
  operations: "Operations",
  support: "Support",
  tester: "Tester",
  robot: "Robot",
  custom: "Custom",
};

/** The info control beside the actor's name. Opens the read-only details
 *  and (for the owning actor) the supported edit affordances. */
export function AssistantAboutButton({ className = "" }: { className?: string }) {
  const t = useTranslate();
  const actor = useAssistantActor();
  const [open, setOpen] = useState(false);
  const label = actor.scope === null ? t("About the assistant") : actor.scope === "workspace" ? t("About the workspace assistant") : t("About your assistant");
  return (
    <>
      <button
        type="button"
        className={`assistant-about-trigger ${className}`.trim()}
        aria-label={label}
        title={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <IconInfo size={14} />
      </button>
      {open && <AssistantAboutDialog actor={actor} onClose={() => setOpen(false)} />}
    </>
  );
}

function focusables(root: HTMLElement | null): HTMLElement[] {
  if (root === null) return [];
  return Array.from(
    root.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
    ),
  ).filter((el) => !el.matches(":disabled") && el.getAttribute("aria-hidden") !== "true");
}

function AssistantAboutDialog({ actor, onClose }: { actor: AssistantActorValue; onClose: () => void }) {
  const model = actor.model;
  const t = useTranslate();
  const titleId = useId();
  const panelRef = useRef<HTMLElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const read = actor.read;
  const identity = read.state === "ready" ? read.identity : null;
  const title = actor.scope === null ? t("About the assistant") : actor.scope === "workspace" ? t("About the workspace assistant") : t("About your assistant");

  useEffect(() => {
    openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();
    return () => {
      const opener = openerRef.current;
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  // Saving can disable the focused submit button and leave focus on body.
  // The open dialog still owns Escape and Tab in that state.
  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const items = focusables(panelRef.current);
      if (items.length === 0) {
        event.preventDefault();
        panelRef.current?.focus();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      if (!panelRef.current?.contains(active)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      } else if (event.shiftKey) {
        if (active === first || active === panelRef.current) {
          event.preventDefault();
          last.focus();
        }
      } else if (active === last || active === panelRef.current) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  return createPortal(
    <div className="assistant-about-wrap">
      <div className="assistant-about-scrim" onClick={onClose} />
      <aside
        ref={panelRef}
        className="assistant-about card"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        data-assistant-identity={actor.scope}
        data-identity-state={read.state}
      >
        <header className="assistant-about-head">
          <h2 id={titleId} className="text-cardtitle font-medium text-ink-100">
            {title}
          </h2>
          <button type="button" className="assistant-about-trigger" aria-label={t("Close assistant details")} title={t("Close assistant details")} onClick={onClose}>
            <IconClose size={13} />
          </button>
        </header>

        <div className="assistant-about-who">
          <AssistantFace size={44} />
          <div className="min-w-0">
            <h3 className="assistant-about-name" data-assistant-name>
              {displayName(read, actor.scope, t)}
            </h3>
            <p className="assistant-scope">{scopeDescriptor(actor.scope, t)}</p>
          </div>
        </div>

        {read.state === "unavailable" && (
          <p className="assistant-about-warn" role="status">
            {t("The saved identity couldn’t be read. This is not the same as an unnamed assistant.")}
          </p>
        )}

        <dl className="assistant-about-facts">
          <div>
            <dt>{t("Display name")}</dt>
            <dd>{identity === null ? t("Unavailable") : identity.name ?? t("Not named yet")}</dd>
          </div>
          <div>
            <dt>{t("Personality")}</dt>
            <dd>{identity === null ? t("Unavailable") : identity.tone ?? t("Neutral, clear and practical")}</dd>
          </div>
          <div>
            <dt>{t("Belongs to")}</dt>
            <dd>{actor.scope === null ? t("Unavailable") : actor.scope === "workspace" ? t("This workspace") : t("You")}</dd>
          </div>
          {model && (
            <div data-assistant-model>
              <dt>{t("Model")}</dt>
              <dd className="break-words">{model}</dd>
            </div>
          )}
        </dl>

        {actor.scope === "personal" ? (
          <PersonalIdentityDetails actor={actor} identity={identity} />
        ) : actor.scope === "workspace" ? (
          <WorkspaceIdentityDetails actor={actor} identity={identity} />
        ) : null}
      </aside>
    </div>,
    document.body,
  );
}

const QUICK_NAMES = ["Nova", "Atlas", "Kai", "Milo"] as const;

export interface HostedPersonalBootstrap {
  bootstrap: string | null;
  identity: IdentityRecord | null;
  resuming: boolean;
  complete: (input: {
    expected_revision: number;
    name?: string;
    look: AvatarLook;
  }) => Promise<PersonalProfileOutcome>;
  defer: (expected_revision: number) => Promise<PersonalProfileOutcome>;
  resume: () => Promise<void>;
}

/** CAD-2269: single-flight personal bootstrap read + fenced mutations for hosted
 *  MasterHome (where `GET /api/master/state` projects `identity` without
 *  `bootstrap_state`). Scoped by session and conversation; disabled on native boards. */
export function useHostedPersonalBootstrap(
  enabled: boolean,
  conversation: string | null,
  observedRevision: number | null,
): HostedPersonalBootstrap {
  const scopeKey = `${sessionKey() ?? ""}:${privateSessionGeneration()}:${conversation ?? ""}`;
  const [state, setState] = useState<{
    scope: string;
    bootstrap: string | null;
    identity: IdentityRecord | null;
  } | null>(null);
  const [resuming, setResuming] = useState(false);
  const gate = useRef<{ inflight: boolean; again: boolean; scope: string }>({
    inflight: false,
    again: false,
    scope: "",
  });
  const alive = useRef(true);
  const current = state !== null && state.scope === scopeKey ? state : null;

  const refreshMaster = useCallback(() => {
    void resources.masterState.refresh();
    if (conversation !== null) void resources.masterConversation(conversation).refresh();
  }, [conversation]);

  const load = useCallback(() => {
    if (!enabled) return;
    const g = gate.current;
    if (g.inflight && g.scope === scopeKey) {
      g.again = true;
      return;
    }
    g.inflight = true;
    g.scope = scopeKey;
    api
      .memberAssistState()
      .then((next) => {
        if (!alive.current || gate.current.scope !== scopeKey) return;
        const parsed = readPersonalIdentity(next);
        const bootstrap = readPersonalBootstrap(next);
        setState({
          scope: scopeKey,
          bootstrap,
          identity: parsed.state === "ready" ? parsed.identity : null,
        });
      })
      .catch(() => undefined)
      .finally(() => {
        if (gate.current.scope !== scopeKey) return;
        gate.current.inflight = false;
        if (gate.current.again && alive.current) {
          gate.current.again = false;
          load();
        }
      });
  }, [enabled, scopeKey]);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    if (!enabled) return;
    load();
  }, [enabled, load, observedRevision]);

  const runMutation = useCallback(
    async (req: PersonalProfileMutation): Promise<PersonalProfileOutcome> => {
      const outcome = await postPersonalProfile(req);
      if (outcome.ok) {
        setState({
          scope: scopeKey,
          bootstrap: outcome.bootstrap,
          identity: outcome.identity,
        });
        refreshMaster();
        return outcome;
      }
      if (outcome.conflict) {
        load();
        refreshMaster();
      }
      return outcome;
    },
    [scopeKey, refreshMaster, load],
  );

  const complete = useCallback(
    (input: { expected_revision: number; name?: string; look: AvatarLook }) =>
      runMutation(toPersonalCompleteMutation(input)),
    [runMutation],
  );

  const defer = useCallback(
    (expected_revision: number) => runMutation({ expected_revision, action: "defer" }),
    [runMutation],
  );

  const resume = useCallback(async () => {
    if (current === null || current.identity === null || resuming) return;
    setResuming(true);
    await runMutation({ expected_revision: Number(current.identity.revision), action: "resume" });
    setResuming(false);
  }, [current, resuming, runMutation]);

  return useMemo<HostedPersonalBootstrap>(
    () => ({
      bootstrap: current?.bootstrap ?? null,
      identity: current?.identity ?? null,
      resuming,
      complete,
      defer,
      resume,
    }),
    [current, resuming, complete, defer, resume],
  );
}

/** CAD-2269: minimal first-entry setup card for the user's personal assistant.
 *  Avatar + Name + Skip for now — zero required chat turns. */
export function AssistantBootstrapCard({
  identity,
  onComplete,
  onDefer,
}: {
  identity: IdentityRecord;
  onComplete: (input: {
    expected_revision: number;
    name?: string;
    look: AvatarLook;
  }) => Promise<PersonalProfileOutcome>;
  onDefer: (expected_revision: number) => Promise<PersonalProfileOutcome>;
}) {
  const t = useTranslate();
  const headingId = useId();
  const [base, setBase] = useState(identity);
  const [name, setName] = useState(identity.name ?? "");
  const [look, setLook] = useState<AvatarLook>(identity.look);
  const [busy, setBusy] = useState<"complete" | "defer" | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const trimmed = name.trim();
  const dirty = (trimmed === "" ? null : trimmed) !== base.name || look !== base.look;
  const stale = Number(identity.revision) > Number(base.revision);

  useEffect(() => {
    if (stale && !dirty) {
      setBase(identity);
      setName(identity.name ?? "");
      setLook(identity.look);
    }
  }, [stale, dirty, identity]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy !== null || stale || unsupported) return;
    setBusy("complete");
    setError(null);
    const outcome = await onComplete({
      expected_revision: Number(base.revision),
      ...(trimmed !== "" ? { name: trimmed } : {}),
      look,
    });
    setBusy(null);
    if (!outcome.ok) {
      if (outcome.unsupported) setUnsupported(true);
      setError(t(outcome.error));
    }
  };

  const skip = async () => {
    if (busy !== null || unsupported) return;
    setBusy("defer");
    setError(null);
    const outcome = await onDefer(Number(identity.revision));
    setBusy(null);
    if (!outcome.ok) {
      if (outcome.unsupported) setUnsupported(true);
      setError(t(outcome.error));
    }
  };

  return (
    <section
      className="card assistant-bootstrap"
      aria-labelledby={headingId}
      data-assistant-bootstrap="pending"
    >
      <div className="assistant-bootstrap-head">
        <span className="assistant-face">
          <AgentAvatar slug={look} size={36} still />
        </span>
        <div className="min-w-0">
          <h2 id={headingId} className="assistant-bootstrap-title">
            {t("Name your assistant")}
          </h2>
          <p className="assistant-scope">{t("Private to you in this workspace.")}</p>
        </div>
      </div>

      <form className="assistant-edit" onSubmit={(event) => void submit(event)}>
        <fieldset className="assistant-looks" disabled={busy !== null || unsupported}>
          <legend className="text-label text-ink-300">{t("Avatar")}</legend>
          <div className="assistant-look-grid">
            {AVATAR_LOOKS.map((option) => (
              <button
                key={option}
                type="button"
                className="assistant-look"
                aria-pressed={look === option}
                aria-label={t(LOOK_LABEL[option])}
                title={t(LOOK_LABEL[option])}
                data-assistant-look={option}
                onClick={() => setLook(option)}
              >
                <span className="assistant-face">
                  <AgentAvatar slug={option} size={34} still />
                </span>
              </button>
            ))}
          </div>
        </fieldset>

        <div className="assistant-field">
          <label className="assistant-field">
            <span>{t("Name")}</span>
            <input
              className="field"
              value={name}
              maxLength={80}
              disabled={busy !== null || unsupported}
              autoComplete="off"
              placeholder={t("e.g. Nova")}
              onChange={(event) => setName(event.target.value)}
            />
          </label>
          <div className="assistant-name-chips">
            {QUICK_NAMES.map((preset) => (
              <button
                key={preset}
                type="button"
                className="chip assistant-name-chip"
                aria-pressed={trimmed === preset}
                disabled={busy !== null || unsupported}
                onClick={() => setName(preset)}
              >
                {preset}
              </button>
            ))}
          </div>
        </div>

        {stale && (
          <>
            <p className="assistant-about-pending" role="status">
              {t("Your assistant identity changed elsewhere. Reload before saving.")}
            </p>
            <div>
              <Button
                type="button"
                size="sm"
                disabled={busy !== null || unsupported}
                onClick={() => {
                  setBase(identity);
                  setName(identity.name ?? "");
                  setLook(identity.look);
                  setError(null);
                }}
              >
                {t("Reload identity")}
              </Button>
            </div>
          </>
        )}

        {error !== null && (
          <p className="assistant-about-error" role="alert">
            {error}
          </p>
        )}

        <div className="assistant-bootstrap-actions">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            loading={busy === "defer"}
            disabled={busy !== null || unsupported}
            onClick={() => void skip()}
          >
            {t("Skip for now")}
          </Button>
          <Button
            type="submit"
            size="sm"
            variant="primary"
            loading={busy === "complete"}
            disabled={busy !== null || stale || unsupported}
          >
            {busy === "complete"
              ? t("Saving…")
              : trimmed !== ""
                ? t("Start with {name}").replace("{name}", trimmed)
                : t("Save assistant")}
          </Button>
        </div>
      </form>
    </section>
  );
}

function PersonalIdentityDetails({
  actor,
  identity,
}: {
  actor: AssistantActorValue;
  identity: IdentityRecord | null;
}) {
  const t = useTranslate();
  const request = actor.personal;
  const [directUnsupported, setDirectUnsupported] = useState(false);
  const [fallbackNote, setFallbackNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);

  if (request?.saveProfile !== undefined && identity !== null && !directUnsupported) {
    return (
      <PersonalIdentityEditor
        request={request}
        identity={identity}
        onUnsupported={(lookChanged) => {
          setDirectUnsupported(true);
          setFallbackNote(
            t("Direct profile edits aren’t available on this board yet. Ask in chat to change my name."),
          );
          if (lookChanged !== null) void request.requestLook(lookChanged);
        }}
      />
    );
  }

  const pick = async (look: AvatarLook) => {
    if (request === null || sending) return;
    setError(null);
    setSending(true);
    const result = await request.requestLook(look);
    setSending(false);
    if (!result.ok) setError(result.error ?? t("The avatar request was not sent."));
  };

  return (
    <>
      <p className="assistant-about-note">
        {fallbackNote ?? t("Ask me in this conversation to change my name or how I communicate. Your history and access stay the same.")}
      </p>
      {request === null && (
        <p className="assistant-about-note">{t("Avatar changes are unavailable on a read-only board.")}</p>
      )}
      <fieldset className="assistant-looks" disabled={request === null || identity === null}>
        <legend className="text-label text-ink-300">{t("Avatar")}</legend>
        <div className="assistant-look-grid">
          {AVATAR_LOOKS.map((look) => (
            <button
              key={look}
              type="button"
              className="assistant-look"
              aria-pressed={identity?.look === look}
              aria-label={t(LOOK_LABEL[look])}
              title={t(LOOK_LABEL[look])}
              data-assistant-look={look}
              disabled={sending}
              onClick={() => void pick(look)}
            >
              <span className="assistant-face">
                <AgentAvatar slug={look} size={34} still />
              </span>
            </button>
          ))}
        </div>
      </fieldset>
      {request?.pendingLook != null && (
        <p className="assistant-about-pending" role="status">
          {t("Avatar change requested. It appears here only after the assistant confirms and saves it.")}
        </p>
      )}
      {error !== null && (
        <p className="assistant-about-error" role="alert">
          {error}
        </p>
      )}
    </>
  );
}

function PersonalIdentityEditor({
  request,
  identity,
  onUnsupported,
}: {
  request: PersonalLookRequest;
  identity: IdentityRecord;
  onUnsupported: (lookChanged: AvatarLook | null) => void;
}) {
  const t = useTranslate();
  const [base, setBase] = useState(identity);
  const [name, setName] = useState(identity.name ?? "");
  const [look, setLook] = useState<AvatarLook>(identity.look);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const stale = Number(identity.revision) > Number(base.revision);

  const nextName = name.trim() === "" ? null : name.trim();
  const dirty = nextName !== base.name || look !== base.look;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!request.saveProfile || pending || !dirty || stale) return;
    setPending(true);
    setError(null);
    setNotice(null);
    const result = await request.saveProfile({
      expected_revision: Number(base.revision),
      ...(nextName !== base.name ? { name: nextName ?? "" } : {}),
      ...(look !== base.look ? { look } : {}),
    });
    setPending(false);
    if (!result.ok) {
      if (result.unsupported) {
        onUnsupported(look !== base.look ? look : null);
        return;
      }
      setError(t(result.error));
      return;
    }
    setBase(result.identity);
    setName(result.identity.name ?? "");
    setLook(result.identity.look);
    setNotice(t("Saved. The new identity applies to the next turn."));
  };

  return (
    <form className="assistant-edit" onSubmit={(event) => void submit(event)}>
      <p className="assistant-about-note">
        {t("Private to you in this workspace. Ask in chat anytime to adjust how I communicate.")}
      </p>
      <label className="assistant-field">
        <span>{t("Display name")}</span>
        <input
          className="field"
          value={name}
          maxLength={80}
          disabled={pending}
          autoComplete="off"
          onChange={(event) => setName(event.target.value)}
          placeholder={t("Not named yet")}
        />
      </label>
      <fieldset className="assistant-looks" disabled={pending}>
        <legend className="text-label text-ink-300">{t("Avatar")}</legend>
        <div className="assistant-look-grid">
          {AVATAR_LOOKS.map((option) => (
            <button
              key={option}
              type="button"
              className="assistant-look"
              aria-pressed={look === option}
              aria-label={t(LOOK_LABEL[option])}
              title={t(LOOK_LABEL[option])}
              data-assistant-look={option}
              onClick={() => setLook(option)}
            >
              <span className="assistant-face">
                <AgentAvatar slug={option} size={34} still />
              </span>
            </button>
          ))}
        </div>
      </fieldset>
      {stale && (
        <>
          <p className="assistant-about-pending" role="status">
            {t("Your assistant identity changed elsewhere. Your draft has been kept. Reload before editing again.")}
          </p>
          <Button
            type="button"
            size="sm"
            disabled={pending}
            onClick={() => {
              setBase(identity);
              setName(identity.name ?? "");
              setLook(identity.look);
              setError(null);
              setNotice(null);
            }}
          >
            {t("Reload identity")}
          </Button>
        </>
      )}
      {error !== null && (
        <p className="assistant-about-error" role="alert">
          {`${t("The save could not be confirmed. Refresh to check the saved identity; it was not retried.")} ${error}`}
        </p>
      )}
      {notice !== null && (
        <p className="assistant-about-pending" role="status">
          {notice}
        </p>
      )}
      <Button type="submit" size="sm" variant="primary" loading={pending} disabled={pending || !dirty || stale}>
        {pending ? t("Saving…") : t("Save identity")}
      </Button>
    </form>
  );
}

function WorkspaceIdentityDetails({
  actor,
  identity,
}: {
  actor: AssistantActorValue;
  identity: IdentityRecord | null;
}) {
  const t = useTranslate();
  const workspace = actor.workspace;
  return (
    <>
      <p className="assistant-about-note">
        {t("This identity belongs to the workspace, not to its current owner. Changing it never changes permissions.")}
      </p>
      {workspace?.canEdit === true && identity !== null ? (
        <WorkspaceIdentityEditor actor={actor} identity={identity} />
      ) : identity === null ? null : (
        <p className="assistant-about-note">{t("Only an operator can change the workspace identity.")}</p>
      )}
    </>
  );
}

function WorkspaceIdentityEditor({ actor, identity }: { actor: AssistantActorValue; identity: IdentityRecord }) {
  const t = useTranslate();
  const [base, setBase] = useState(identity);
  const [name, setName] = useState(identity.name ?? "");
  const [tone, setTone] = useState(identity.tone ?? "");
  const [look, setLook] = useState<AvatarLook>(identity.look);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Keep the editor's revision with its draft. A live resource update must
  // never discard unsaved text or silently authorize it against a newer base.
  const stale = String(identity.revision) !== String(base.revision);

  const nextName = name.trim() === "" ? null : name.trim();
  const nextTone = tone.trim() === "" ? null : tone.trim();
  const dirty = nextName !== base.name || nextTone !== base.tone || look !== base.look;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const workspace = actor.workspace;
    if (workspace === null || pending || !dirty) return;
    setPending(true);
    setError(null);
    setNotice(null);
    const result = await workspace.save({
      expected_revision: String(base.revision),
      ...(nextName !== base.name ? { name: nextName ?? "" } : {}),
      ...(nextTone !== base.tone ? { tone: nextTone ?? "" } : {}),
      ...(look !== base.look ? { look } : {}),
    });
    setPending(false);
    if (!result.ok) {
      setError(result.error);
      return;
    }
    setBase(result.identity);
    setName(result.identity.name ?? "");
    setTone(result.identity.tone ?? "");
    setLook(result.identity.look);
    setNotice(t("Saved. The new identity applies to the next turn."));
  };

  return (
    <form className="assistant-edit" onSubmit={(event) => void submit(event)}>
      <p className="text-label text-ink-400">
        {t("Operator-only edit. Your personal assistant keeps its own name and preferences.")}
      </p>
      <label className="assistant-field">
        <span>{t("Display name")}</span>
        <input
          className="field"
          value={name}
          maxLength={80}
          disabled={pending}
          autoComplete="off"
          onChange={(event) => setName(event.target.value)}
          placeholder={t("Not named yet")}
        />
      </label>
      <label className="assistant-field">
        <span>{t("Personality")}</span>
        <textarea
          className="field"
          value={tone}
          rows={3}
          maxLength={240}
          disabled={pending}
          onChange={(event) => setTone(event.target.value)}
          placeholder={t("Neutral, clear and practical")}
        />
      </label>
      <fieldset className="assistant-looks" disabled={pending}>
        <legend className="text-label text-ink-300">{t("Avatar")}</legend>
        <div className="assistant-look-grid">
          {AVATAR_LOOKS.map((option) => (
            <button
              key={option}
              type="button"
              className="assistant-look"
              aria-pressed={look === option}
              aria-label={t(LOOK_LABEL[option])}
              title={t(LOOK_LABEL[option])}
              data-assistant-look={option}
              onClick={() => setLook(option)}
            >
              <span className="assistant-face">
                <AgentAvatar slug={option} size={34} still />
              </span>
            </button>
          ))}
        </div>
      </fieldset>
      {stale && (
        <>
          <p className="assistant-about-pending" role="status">
            {t("The workspace identity changed elsewhere. Your draft has been kept. Reload the saved identity before editing again.")}
          </p>
          <Button type="button" size="sm" disabled={pending} onClick={() => {
            setBase(identity);
            setName(identity.name ?? "");
            setTone(identity.tone ?? "");
            setLook(identity.look);
            setError(null);
            setNotice(null);
          }}>
            {t("Reload identity")}
          </Button>
        </>
      )}
      {error !== null && (
        <p className="assistant-about-error" role="alert">
          {`${t("The save could not be confirmed. Refresh to check the saved identity; it was not retried.")} ${error}`}
        </p>
      )}
      {notice !== null && (
        <p className="assistant-about-pending" role="status">
          {notice}
        </p>
      )}
      <Button type="submit" size="sm" variant="primary" loading={pending} disabled={pending || !dirty}>
        {pending ? t("Saving…") : t("Save identity")}
      </Button>
    </form>
  );
}
