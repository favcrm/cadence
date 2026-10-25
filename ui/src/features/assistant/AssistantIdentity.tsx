/** CAD-1817: the shared assistant-identity UI. One actor is bound to the
 *  pane it labels — the private assistant on the member surface, the
 *  workspace coordinator on Master/app panes — and every heading, sender
 *  row, composer label and About panel reads that actor, never a global
 *  translation or another user's profile.
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
  readWorkspaceIdentity,
  scopeDescriptor,
  WORKSPACE_ALIAS,
  type AssistantScope,
  type AvatarLook,
  type IdentityRead,
  type IdentityRecord,
} from "./identity";
import "./assistant-identity.css";

/** One explicit personal-avatar request through the existing private
 *  chat. `pendingLook` never becomes the displayed identity: only an
 *  authorized state refresh can do that. */
export interface PersonalLookRequest {
  requestLook: (look: AvatarLook) => Promise<{ ok: boolean; error?: string }>;
  pendingLook: AvatarLook | null;
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
  scope: AssistantScope;
  /** The stable agent alias that anchors the unpinned avatar look. */
  alias: string;
  read: IdentityRead;
  personal: PersonalLookRequest | null;
  workspace: {
    canEdit: boolean;
    save: (change: WorkspaceIdentityChange) => Promise<WorkspaceSaveResult>;
  } | null;
}

/** Outside a provider (e.g. a human thread's bubbles) the historical
 *  generic role label stands; nothing claims a personal name there. */
const FALLBACK_ACTOR: AssistantActorValue = {
  scope: "personal",
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

/** The workspace coordinator's identity projection plus the operator's
 *  guarded edit. The save's own response identity is shown until a state
 *  read reflects a different revision (this save or another operator's),
 *  so a save never flickers back to the pre-save name while the refresh
 *  is in flight, and a newer committed revision still wins. */
export function useWorkspaceActor(master: ResourceState<MasterState>, canEdit: boolean): AssistantActorValue {
  const [saved, setSaved] = useState<{ identity: IdentityRecord; before: string; scope: string } | null>(null);
  // A save may only be shown within the private session that made it: a
  // user/workspace switch invalidates the overlay immediately, even if the
  // resource still carries the previous principal's payload.
  const scopeKey = `${sessionKey() ?? ""}:${privateSessionGeneration()}`;
  const scoped = saved !== null && saved.scope === scopeKey ? saved : null;
  const live: IdentityRead =
    master.data !== null
      ? readWorkspaceIdentity(master.data)
      : master.status === "failed"
        ? { state: "unavailable" }
        : { state: "loading" };

  const liveCommitted =
    scoped !== null && live.state === "ready" && String(live.identity.revision) !== scoped.before;
  useEffect(() => {
    if (liveCommitted) setSaved(null);
  }, [liveCommitted]);

  const read: IdentityRead =
    scoped !== null && !liveCommitted ? { state: "ready", identity: scoped.identity } : live;

  const save = useCallback(
    async (change: WorkspaceIdentityChange): Promise<WorkspaceSaveResult> => {
      if (read.state !== "ready") {
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
        void resources.masterState.refresh();
        try {
          if (result.applies !== "next_turn") {
            throw new Error("The saved workspace identity response was not recognized.");
          }
          const identity = decodeIdentity(result.identity, "workspace");
          setSaved({ identity, before, scope: scopeKey });
          return { ok: true, identity };
        } catch (error) {
          return { ok: false, error: String(error) };
        }
      } catch (error) {
        // HTTP status alone cannot prove no effect across the file/store
        // writes. Preserve the draft and reconcile from committed state.
        void resources.masterState.refresh();
        return {
          ok: false,
          error: error instanceof ApiError ? error.message : String(error),
        };
      }
    },
    [read, scopeKey],
  );

  return useMemo<AssistantActorValue>(
    () => ({ scope: "workspace", alias: WORKSPACE_ALIAS, read, personal: null, workspace: { canEdit, save } }),
    [read, canEdit, save],
  );
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
  const label = actor.scope === "workspace" ? t("About the workspace assistant") : t("About your assistant");
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
  const t = useTranslate();
  const titleId = useId();
  const panelRef = useRef<HTMLElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const read = actor.read;
  const identity = read.state === "ready" ? read.identity : null;
  const title = actor.scope === "workspace" ? t("About the workspace assistant") : t("About your assistant");

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
            <dd>{actor.scope === "workspace" ? t("This workspace") : t("You")}</dd>
          </div>
        </dl>

        {actor.scope === "personal" ? (
          <PersonalIdentityDetails actor={actor} identity={identity} />
        ) : (
          <WorkspaceIdentityDetails actor={actor} identity={identity} />
        )}
      </aside>
    </div>,
    document.body,
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
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);

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
        {t("Ask me in this conversation to change my name or how I communicate. Your history and access stay the same.")}
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
