import { useCallback, useContext, useEffect, useRef, useState, type FormEvent } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { resources } from "../../lib/resources";
import type { PersonalAssistantRow } from "../../lib/types";
import Button from "../../ui/Button";
import { IconClose, IconEdit } from "../../ui/icons";
import type { ToastMsg } from "../../ui/Toast";
import { WriteGate } from "../auth/WriteGate";
import {
  LOOK_LABEL,
  postPersonalProfile,
  toPersonalUpdateMutation,
} from "../assistant/AssistantIdentity";
import {
  AVATAR_LOOKS,
  readPersonalIdentity,
  type AvatarLook,
  type IdentityRecord,
} from "../assistant/identity";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { OptInTools, WorkspaceSkills, type TabContext } from "./AgentTabs";
import type { AgentTab } from "./agentTabModel";
import AssistantMemoryTab from "./AssistantMemoryTab";
import InstructionsEditor from "./InstructionsEditor";
import { contextOf } from "./instructionsDoc";
import { PERSONAL_ALIAS } from "./personalAssistant";
import RoutinesTab from "./RoutinesTab";
import { useInstructionsDoc, type InstructionsDoc } from "./useInstructionsDoc";
import "./agents.css";
import "../assistant/assistant-identity.css";

const TONE_PRESETS = [
  "Concise & direct",
  "Warm & helpful",
  "Structured & analytical",
] as const;

const PERSONAL_TABS = [
  { id: "instructions", label: "Instructions" },
  { id: "memory", label: "Memory" },
  { id: "routines", label: "Routines" },
  { id: "skills", label: "Skills" },
  { id: "tools", label: "Tools" },
] as const;

type PersonalTab = (typeof PERSONAL_TABS)[number]["id"];

function normalizePersonalTab(tab: AgentTab | undefined): PersonalTab {
  if (
    tab === "memory" ||
    tab === "routines" ||
    tab === "skills" ||
    tab === "tools" ||
    tab === "instructions"
  ) {
    return tab;
  }
  return "instructions";
}

function personalTabContext({
  alias,
  displayName,
  operator,
  writable,
  hostedBoard,
  scopedRoster,
  doc,
  toast,
}: {
  alias: string;
  displayName: string;
  operator: boolean;
  writable: boolean;
  hostedBoard: boolean;
  scopedRoster: boolean;
  doc: InstructionsDoc;
  toast: (kind: ToastMsg["kind"], text: string) => void;
}): TabContext {
  return {
    alias,
    personalAssistant: true,
    displayName,
    profile: undefined,
    observed: undefined,
    detail: null,
    role: null,
    hosted: hostedBoard,
    hostedBoard,
    scopedRoster,
    resume: undefined,
    onOpenIssue: () => {},
    hired: undefined,
    operator,
    writable,
    readOnlyReason: null,
    toast,
    stats: [],
    detailFailed: false,
    detailNotServed: false,
    retryDetail: () => {},
    roleRead: "read",
    retryRole: () => {},
    doc,
  };
}

/** CAD-2353: `/agents/assistant`, the caller's own assistant. The roster row
 *  names it; the header and routines key refine name, face and `assistant_id`
 *  from the member's own `/api/member-assistant/state` (never `/api/agents/assistant`).
 *  Its chat is Home. Its tabs align with agent pages: Instructions, Routines
 *  (with Incoming endpoints), Skills, and Tools (owner-gated opt-in groups). */
export default function MyAssistantPage({
  row,
  tab,
  onTab,
  operator = false,
  hostedBoard = true,
  scopedRoster = false,
  onBack,
  onToast,
  rosterStale = false,
}: {
  row: PersonalAssistantRow;
  tab?: AgentTab;
  onTab?: (next: AgentTab) => void;
  operator?: boolean;
  hostedBoard?: boolean;
  scopedRoster?: boolean;
  onBack: () => void;
  onToast: (kind: ToastMsg["kind"], text: string) => void;
  /** The roster shows its last answer because the board isn't answering now. */
  rosterStale?: boolean;
}) {
  const t = useTranslate();
  const boardBlock = useContext(WriteGate);
  const writable = boardBlock === null && !rosterStale;
  const [localTab, setLocalTab] = useState<PersonalTab>("instructions");
  const section = onTab ? normalizePersonalTab(tab) : tab && tab !== "overview" ? normalizePersonalTab(tab) : localTab;
  const selectTab = (next: PersonalTab) => {
    setLocalTab(next);
    onTab?.(next);
  };

  const doc = useInstructionsDoc({
    alias: PERSONAL_ALIAS,
    kind: "personal",
    profileId: null,
    nativeServed: null,
    writable,
    onSaved: () => undefined,
    toast: onToast,
  });
  const context = contextOf(doc.draft ?? doc.served ?? "", doc.kind, doc.applies, doc.builtInInstructionTokens);
  const [ownIdentity, setOwnIdentity] = useState<IdentityRecord | null>(null);
  const [assistantId, setAssistantId] = useState<string | null>(null);
  const [assistantRead, setAssistantRead] = useState<"reading" | "ready" | "failed">("reading");
  const [assistantAttempt, setAssistantAttempt] = useState(0);

  const [avatarOpen, setAvatarOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [draftTone, setDraftTone] = useState("");
  const [saving, setSaving] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);
  const [conflictReload, setConflictReload] = useState(false);
  const [savedBadge, setSavedBadge] = useState<string | null>(null);
  const [directUnsupported, setDirectUnsupported] = useState(false);
  const avatarWrapRef = useRef<HTMLDivElement | null>(null);
  const avatarBtnRef = useRef<HTMLButtonElement | null>(null);
  const changeAvatarLinkRef = useRef<HTMLButtonElement | null>(null);
  const nameInputRef = useRef<HTMLInputElement | null>(null);
  const toneInputRef = useRef<HTMLInputElement | null>(null);

  const loadPersonalState = useCallback(async () => {
    const state = await api.memberAssistState();
    const read = readPersonalIdentity(state);
    if (read.state === "ready") {
      setOwnIdentity(read.identity);
      doc.syncOwnRevision?.(read.identity.revision);
    }
    return { state, read };
  }, [doc.syncOwnRevision]);

  useEffect(() => {
    let live = true;
    setAssistantRead((cur) => (cur === "ready" ? cur : "reading"));
    api
      .memberAssistState()
      .then((state) => {
        if (!live) return;
        const read = readPersonalIdentity(state);
        if (read.state === "ready") {
          setOwnIdentity(read.identity);
        }
        const id = state.assistant?.assistant_id;
        if (typeof id === "string" && id.trim().length > 0) {
          setAssistantId(id);
          setAssistantRead("ready");
        } else {
          setAssistantId(null);
          setAssistantRead("failed");
        }
      })
      // The roster's name and face already read; a failed refinement keeps them.
      .catch(() => {
        if (live) setAssistantRead("failed");
      });
    return () => {
      live = false;
    };
  }, [assistantAttempt]);

  useEffect(() => {
    if (!avatarOpen && !editing) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (avatarOpen) {
        event.stopPropagation();
        setAvatarOpen(false);
        queueMicrotask(() => avatarBtnRef.current?.focus());
      } else if (editing) {
        event.stopPropagation();
        setEditing(false);
        if (!conflictReload) {
          setEditError(null);
        }
      }
    };
    const onPointerDown = (event: Event) => {
      const target = event.target as Node | null;
      if (
        avatarOpen &&
        target !== null &&
        avatarWrapRef.current &&
        !avatarWrapRef.current.contains(target) &&
        !(changeAvatarLinkRef.current?.contains(target) ?? false)
      ) {
        setAvatarOpen(false);
      }
    };
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [avatarOpen, conflictReload, editing]);

  const docRev = typeof doc.ownRevision === "number" ? doc.ownRevision : null;
  const ownRev = ownIdentity !== null && typeof ownIdentity.revision === "number" ? ownIdentity.revision : null;
  const effectiveRevision =
    ownRev !== null && docRev !== null ? Math.max(ownRev, docRev) : (ownRev ?? docRev);
  const canEditIdentity =
    writable &&
    hostedBoard &&
    ownIdentity !== null &&
    effectiveRevision !== null &&
    !directUnsupported;

  const commitIdentity = (next: IdentityRecord, toastText: string, badgeText: string) => {
    setOwnIdentity(next);
    doc.syncOwnRevision?.(next.revision);
    setConflictReload(false);
    setEditError(null);
    setSavedBadge(badgeText);
    onToast("ok", toastText);
    void resources.agents.refresh();
    void resources.masterState.refresh();
  };

  const reloadIdentityFromConflict = async () => {
    if (saving) return false;
    setSaving(true);
    try {
      const { read } = await loadPersonalState();
      if (read.state !== "ready") {
        setConflictReload(true);
        const message = t("Unable to reload assistant profile. Try again.");
        setEditError(message);
        onToast("err", message);
        return false;
      }
      if (editing) {
        setDraftName(read.identity.name ?? "");
        setDraftTone(read.identity.tone ?? "");
      }
      setEditError(null);
      setConflictReload(false);
      return true;
    } catch {
      setConflictReload(true);
      const message = t("Unable to reload assistant profile. Try again.");
      setEditError(message);
      onToast("err", message);
      return false;
    } finally {
      setSaving(false);
    }
  };

  const pickAvatar = async (nextLook: AvatarLook) => {
    if (!canEditIdentity || ownIdentity === null || effectiveRevision === null || saving || conflictReload) return;
    if (nextLook === ownIdentity.look) {
      setAvatarOpen(false);
      return;
    }
    setSaving(true);
    setEditError(null);
    const outcome = await postPersonalProfile(
      toPersonalUpdateMutation({
        expected_revision: effectiveRevision,
        look: nextLook,
      }),
    );
    if (!outcome.ok) {
      if (outcome.unsupported) {
        setSaving(false);
        setDirectUnsupported(true);
        setAvatarOpen(false);
        onToast("warn", t(outcome.error));
        return;
      }
      if (outcome.conflict) {
        setAvatarOpen(false);
        setConflictReload(true);
        setEditError(t(outcome.error));
        onToast("err", t(outcome.error));
        try {
          const { read } = await loadPersonalState();
          if (read.state === "ready" && !editing) {
            setConflictReload(false);
            setEditError(null);
          }
        } catch {
          // Keep conflictReload active until an explicit reload succeeds.
        } finally {
          setSaving(false);
        }
        return;
      }
      setSaving(false);
      onToast("err", t(outcome.error));
      return;
    }
    setSaving(false);
    setAvatarOpen(false);
    commitIdentity(outcome.identity, t("Avatar updated."), t("Saved"));
  };

  const openEdit = (focusTone: boolean) => {
    if (!canEditIdentity || ownIdentity === null || conflictReload) return;
    setDraftName(ownIdentity.name ?? "");
    setDraftTone(ownIdentity.tone ?? "");
    setEditError(null);
    setConflictReload(false);
    setSavedBadge(null);
    setEditing(true);
    queueMicrotask(() => {
      (focusTone ? toneInputRef.current : nameInputRef.current)?.focus();
    });
  };

  const saveInlineEdit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canEditIdentity || ownIdentity === null || effectiveRevision === null || saving || conflictReload) return;
    const nextName = draftName.trim() === "" ? null : draftName.trim();
    const nextTone = draftTone.trim() === "" ? null : draftTone.trim();
    const dirty = nextName !== ownIdentity.name || nextTone !== ownIdentity.tone;
    if (!dirty) {
      setEditing(false);
      return;
    }
    setSaving(true);
    setEditError(null);
    setConflictReload(false);
    const outcome = await postPersonalProfile(
      toPersonalUpdateMutation({
        expected_revision: effectiveRevision,
        ...(nextName !== ownIdentity.name ? { name: nextName ?? "" } : {}),
        ...(nextTone !== ownIdentity.tone ? { tone: nextTone ?? "" } : {}),
      }),
    );
    setSaving(false);
    if (!outcome.ok) {
      if (outcome.unsupported) {
        setDirectUnsupported(true);
        setEditing(false);
        onToast("warn", t(outcome.error));
        return;
      }
      if (outcome.conflict) {
        setConflictReload(true);
      }
      setEditError(t(outcome.error));
      return;
    }
    setEditing(false);
    commitIdentity(
      outcome.identity,
      t("Saved. The new identity applies to the next turn."),
      t("Saved"),
    );
  };

  const name = ownIdentity?.name ?? row.name;
  const look = ownIdentity?.look ?? row.look;
  const routinesCtx = personalTabContext({
    alias: assistantId ?? "",
    displayName: name,
    operator,
    writable,
    hostedBoard,
    scopedRoster,
    doc,
    toast: onToast,
  });
  const toolsCtx = personalTabContext({
    alias: PERSONAL_ALIAS,
    displayName: name,
    operator,
    writable,
    hostedBoard,
    scopedRoster,
    doc,
    toast: onToast,
  });

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <div className="ap-hero-shell">
        <header className="ap-hero">
          <div className="ap-avatar-wrap" ref={avatarWrapRef}>
            {canEditIdentity ? (
              <button
                ref={avatarBtnRef}
                type="button"
                className="ap-avatar-btn"
                aria-label={t("Change avatar")}
                title={t("Change avatar")}
                aria-haspopup="dialog"
                aria-expanded={avatarOpen}
                disabled={saving || conflictReload}
                onClick={() => {
                  if (saving || conflictReload) return;
                  setAvatarOpen((v) => !v);
                  setSavedBadge(null);
                }}
              >
                <AgentAvatar slug={agentLook(PERSONAL_ALIAS, { look })} still size={52} />
                <span className="ap-avatar-badge" aria-hidden="true">
                  <IconEdit size={11} />
                </span>
              </button>
            ) : (
              <AgentAvatar slug={agentLook(PERSONAL_ALIAS, { look })} still size={48} />
            )}
            {avatarOpen && canEditIdentity && (
              <div
                className="ap-avatar-popover"
                role="dialog"
                aria-label={t("Choose avatar")}
                data-avatar-popover=""
              >
                <div className="ap-pop-head">
                  <span>{t("Choose avatar")}</span>
                  <button
                    type="button"
                    className="ap-pop-close"
                    aria-label={t("Close")}
                    onClick={() => {
                      setAvatarOpen(false);
                      queueMicrotask(() => avatarBtnRef.current?.focus());
                    }}
                  >
                    <IconClose size={12} />
                  </button>
                </div>
                <div className="ap-pop-grid">
                  {AVATAR_LOOKS.map((option) => (
                    <button
                      key={option}
                      type="button"
                      className="assistant-look"
                      aria-pressed={look === option}
                      aria-label={t(LOOK_LABEL[option])}
                      title={t(LOOK_LABEL[option])}
                      data-assistant-look={option}
                      disabled={saving || conflictReload}
                      onClick={() => void pickAvatar(option)}
                    >
                      <span className="assistant-face">
                        <AgentAvatar slug={option} size={34} still />
                      </span>
                    </button>
                  ))}
                </div>
                <p className="ap-pop-foot">{t("Click an avatar to apply immediately.")}</p>
              </div>
            )}
          </div>
          <div className="min-w-0">
            <div className="ap-hero-title-row">
              <h1>{t("My assistant")}</h1>
              <span className="rc-private-chip">{t("Private to you")}</span>
              {canEditIdentity && !editing && (
                <button
                  type="button"
                  className="ap-edit-btn"
                  data-identity-edit=""
                  aria-expanded={editing}
                  disabled={saving || conflictReload}
                  onClick={() => openEdit(false)}
                >
                  <IconEdit size={12} />
                  <span>{t("Edit")}</span>
                </button>
              )}
              {savedBadge && (
                <span className="ap-saved-pill" role="status">
                  {savedBadge}
                </span>
              )}
            </div>
            <div className="ap-sub wrap-anywhere">
              <span>
                {name} · {t("Your assistant")}
              </span>
              {ownIdentity !== null && (
                <>
                  <span aria-hidden="true">·</span>
                  <span>
                    {t("Tone")}:{" "}
                    {canEditIdentity ? (
                      <button
                        type="button"
                        className="ap-tone-link"
                        data-tone-edit=""
                        disabled={saving || conflictReload}
                        onClick={() => openEdit(true)}
                      >
                        {ownIdentity.tone ?? t("Neutral, clear and practical")}
                      </button>
                    ) : (
                      <span>{ownIdentity.tone ?? t("Neutral, clear and practical")}</span>
                    )}
                  </span>
                </>
              )}
            </div>
          </div>
          <div className="ap-acts">
            <Button href="/?chat=assistant">{t("Open Home")}</Button>
          </div>
        </header>
        {!editing && conflictReload && (
          <div className="ap-inline-edit" data-identity-conflict="" role="alert">
            <p className="assistant-about-error">
              {editError ?? t("Another edit was saved first. Reload before saving.")}
            </p>
            <div className="ap-inline-foot">
              <Button
                type="button"
                size="sm"
                loading={saving}
                disabled={saving}
                onClick={() => void reloadIdentityFromConflict()}
              >
                {t("Reload identity")}
              </Button>
            </div>
          </div>
        )}
        {editing && canEditIdentity && ownIdentity !== null && (
          <form className="ap-inline-edit" data-identity-inline-edit="" onSubmit={(event) => void saveInlineEdit(event)}>
            <div className="ap-inline-head">
              <b>{t("Edit assistant profile")}</b>
              <button
                ref={changeAvatarLinkRef}
                type="button"
                className="ap-link"
                disabled={saving || conflictReload}
                onClick={() => setAvatarOpen((open) => !open)}
              >
                {t("Change avatar…")}
              </button>
            </div>
            <div className="ap-identity-fields">
              <label className="assistant-field">
                <span>{t("Display name")}</span>
                <input
                  ref={nameInputRef}
                  className="field"
                  value={draftName}
                  maxLength={80}
                  disabled={saving}
                  autoComplete="off"
                  placeholder={t("Not named yet")}
                  onChange={(event) => setDraftName(event.target.value)}
                />
              </label>
              <div className="assistant-field">
                <label className="assistant-field">
                  <span>{t("Tone")}</span>
                  <input
                    ref={toneInputRef}
                    className="field"
                    value={draftTone}
                    maxLength={80}
                    disabled={saving}
                    autoComplete="off"
                    placeholder={t("e.g. Concise & direct")}
                    onChange={(event) => setDraftTone(event.target.value)}
                  />
                </label>
                <div className="ap-tone-chips">
                  {TONE_PRESETS.map((preset) => (
                    <button
                      key={preset}
                      type="button"
                      className="ap-tone-chip"
                      aria-pressed={draftTone.trim() === preset}
                      disabled={saving}
                      onClick={() => setDraftTone(preset)}
                    >
                      {t(preset)}
                    </button>
                  ))}
                </div>
              </div>
            </div>
            {editError !== null && (
              <p className="assistant-about-error" role="alert">
                {editError}
              </p>
            )}
            <div className="ap-inline-foot">
              <span className="ap-identity-note">{t("Applies to the next turn.")}</span>
              <span className="ap-doc-spacer" />
              {conflictReload && (
                <Button
                  type="button"
                  size="sm"
                  loading={saving}
                  disabled={saving}
                  onClick={() => void reloadIdentityFromConflict()}
                >
                  {t("Reload identity")}
                </Button>
              )}
              <Button
                type="button"
                size="sm"
                variant="ghost"
                disabled={saving}
                onClick={() => {
                  setEditing(false);
                  if (!conflictReload) {
                    setEditError(null);
                  }
                }}
              >
                {t("Cancel")}
              </Button>
              <Button
                type="submit"
                size="sm"
                variant="primary"
                loading={saving}
                disabled={
                  saving ||
                  conflictReload ||
                  ((draftName.trim() === "" ? null : draftName.trim()) === ownIdentity.name &&
                    (draftTone.trim() === "" ? null : draftTone.trim()) === ownIdentity.tone)
                }
              >
                {saving ? t("Saving…") : t("Save")}
              </Button>
            </div>
          </form>
        )}
      </div>
      <nav className="ap-tabs" role="tablist" aria-label={t("Agent sections")}>
        {PERSONAL_TABS.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            id={`ap-tab-${entry.id}`}
            aria-selected={section === entry.id}
            aria-controls="ap-panel"
            className="ap-tab"
            onClick={() => selectTab(entry.id)}
          >
            {t(entry.label)}
          </button>
        ))}
      </nav>
      <div id="ap-panel" role="tabpanel" aria-labelledby={`ap-tab-${section}`}>
        {section === "memory" ? (
          <div className="ap-body">
            <AssistantMemoryTab writable={writable} onToast={onToast} />
          </div>
        ) : section === "routines" ? (
          assistantRead === "reading" ? (
            <p className="rc-note">{t("Loading routines…")}</p>
          ) : assistantRead === "failed" || !assistantId ? (
            <p className="rc-note" role="alert">
              {t("Your assistant ID could not be loaded.")}{" "}
              <button type="button" className="ap-link" onClick={() => setAssistantAttempt((n) => n + 1)}>
                {t("Retry")}
              </button>
            </p>
          ) : (
            <RoutinesTab key={`routine:${assistantId}`} c={routinesCtx} />
          )
        ) : section === "skills" ? (
          doc.read === "unsupported" ? (
            <p className="rc-note">{t("Your assistant’s skills are not edited here yet.")}</p>
          ) : (
            <div className="ap-body">
              <WorkspaceSkills
                doc={doc}
                writable={writable}
                canManageLibrary={operator && writable}
                agentName={name}
                toast={onToast}
                absent={null}
                scopedRoster={scopedRoster}
              />
            </div>
          )
        ) : section === "tools" ? (
          <div className="ap-body">
            {!operator && doc.read === "ready" && (
              <p className="rc-note">
                {t("You can enable personal-safe tools here. Workspace-level tools are managed by the workspace owner.")}
              </p>
            )}
            <OptInTools c={toolsCtx} />
          </div>
        ) : doc.read === "unsupported" ? (
          <p className="rc-note">{t("Your assistant’s instructions are not edited here yet.")}</p>
        ) : doc.read === "failed" ? (
          <p className="rc-note" role="alert">
            {t("The instructions could not be read.")}{" "}
            <button type="button" className="ap-link" onClick={doc.retry}>
              {t("Retry")}
            </button>
          </p>
        ) : doc.read !== "ready" ? (
          <p className="rc-note">{t("Loading instructions…")}</p>
        ) : (
          <div className="ap-body">
            <InstructionsEditor
              doc={doc}
              writable={writable}
              resetTo={null}
              contextHeading={context.heading}
              contextLines={context.lines}
              contextNote={
                <p className="ap-drawer-lead">
                  {t("Skills in full, wiki pages the agent reads, earlier sessions, and a repository’s own agent instructions file during project work.")}
                </p>
              }
            />
          </div>
        )}
      </div>
    </main>
  );
}
