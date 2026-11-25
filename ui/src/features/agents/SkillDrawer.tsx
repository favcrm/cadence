import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import type { WorkspaceSkillRead, WorkspaceSkillSummary, WorkspaceSkillUser } from "../../lib/types";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import DrawerShell, { type DrawerTab } from "../app-shell/shared/DrawerShell";
import { FilesPanel, InstructionsPanel, OverviewPanel, SkillDraftForm, UsedByPanel } from "./SkillPanels";
import {
  editableReferences,
  extractDescription,
  otherRevision,
  referenceFiles,
  refusal,
  skillPills,
  stripFrontmatter,
  usedByCounts,
  type ProfileSkillOutcome,
  type SkillDeps,
} from "./workspaceSkills";

function Note({ children }: { children: React.ReactNode }) {
  return <p className="rc-note">{children}</p>;
}

function Failed({ text, onRetry }: { text: string; onRetry: () => void }) {
  const t = useTranslate();
  return (
    <p className="rc-note" role="alert">
      {t(text)}{" "}
      <button type="button" className="lnk" onClick={onRetry}>
        {t("Retry")}
      </button>
    </p>
  );
}

/** A status pill, its English text translated and its revision filled in. */
export function SkillPills({ skill }: { skill: WorkspaceSkillSummary }) {
  const t = useTranslate();
  return (
    <>
      {skillPills(skill).map((pill) => (
        <span key={pill.text} className={`chip ${pill.tone === "ok" ? "bg-ok/10 text-ok" : "bg-warn/10 text-warn"}`}>
          {t(pill.text).replace("{n}", String(pill.n))}
        </span>
      ))}
    </>
  );
}

type Confirming = "delete" | "revoke" | { turnOff: WorkspaceSkillUser } | null;

/** One library skill: what an agent would read, who uses it, and the owner's decisions on it. */
export function SkillDrawer({
  name,
  initialRevision,
  initialTab = "overview",
  profileId = null,
  agentName = "",
  writable,
  scopedRoster = false,
  deps,
  toast,
  tick = 0,
  assign,
  turnOff,
  reloadFor,
  onChanged,
  onGone,
  onClose,
}: {
  name: string;
  initialRevision?: number;
  initialTab?: "overview" | "instructions" | "used-by" | "files";
  /** The profile whose Skills tab this is, so its own entry reads "this agent". */
  profileId?: string | null;
  agentName?: string;
  writable: boolean;
  scopedRoster?: boolean;
  deps: SkillDeps;
  toast: (kind: ToastMsg["kind"], text: string) => void;
  /** Changes when who uses a skill did: read it again. */
  tick?: number;
  assign?: { on: boolean | null; canChange: boolean; pending: boolean; toggle: (name: string, on: boolean) => Promise<void> };
  turnOff?: (skill: string, user: WorkspaceSkillUser) => Promise<ProfileSkillOutcome>;
  reloadFor?: (user: WorkspaceSkillUser) => void;
  onChanged?: (skill: WorkspaceSkillSummary) => void;
  onGone?: (name: string) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const [shown, setShown] = useState<number | undefined>(initialRevision);
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<WorkspaceSkillRead | null | "failed">(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draftError, setDraftError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<Confirming>(null);
  const [tab, setTab] = useState<string>(initialTab);
  const [notice, setNotice] = useState<{ text: string; user: WorkspaceSkillUser } | null>(null);
  // Where focus goes once the next read lands: a decision swaps its button
  // (Approve for Revoke), and Retry or a revision switch replaces its own.
  const focusNext = useRef<"approve" | "revoke" | "top" | null>(null);
  const focusTop = () => document.querySelector<HTMLElement>('[data-drawer="skill"] .crm-drawer-title')?.focus();
  useEffect(() => {
    let live = true;
    // The last answer stays on screen until the next one lands, so the
    // focused control is not unmounted under a reload.
    deps
      .read(name, shown)
      .then((data) => {
        if (!live) return;
        setRead(data);
        onChanged?.(data.skill);
      })
      .catch((error: unknown) => {
        if (!live) return;
        if (refusal(error).code === "not_found") {
          toast("warn", t("This skill no longer exists."));
          onGone?.(name);
        } else setRead("failed");
      });
    return () => {
      live = false;
    };
    // Only what is asked starts a read; the callbacks are fresh each render.
  }, [deps, name, shown, attempt, tick]);
  useEffect(() => {
    const want = focusNext.current;
    if (want === null || read === null || read === "failed") return;
    focusNext.current = null;
    if (want === "top") focusTop();
    else (document.getElementById(`wskill-${want}`) ?? document.querySelector<HTMLElement>('[data-drawer="skill"] .crm-drawer-title'))?.focus();
  }, [read]);

  const decide = async (
    job: () => Promise<WorkspaceSkillSummary | null>,
    done: string,
    n: number,
    focus: { done: "approve" | "revoke" | null; failed: "approve" | "revoke" | null },
  ) => {
    if (busy) return;
    setBusy(true);
    try {
      const skill = await job();
      setConfirming(null);
      toast("ok", t(done).replace("{name}", () => name).replace("{n}", String(n)));
      if (skill) {
        onChanged?.(skill);
        focusNext.current = focus.done;
        setAttempt((v) => v + 1);
      } else onGone?.(name);
    } catch (error) {
      const why = refusal(error);
      setConfirming(null);
      toast(why.unconfirmed ? "warn" : "err", t(why.text));
      if (why.code === "not_found") onGone?.(name);
      // The skill moved, or the answer was lost: show what the board holds now.
      else if (why.unconfirmed || why.code === "digest_mismatch" || why.code === "revision_conflict" || why.code === "approved_skill") {
        focusNext.current = focus.failed;
        setShown(undefined);
        setAttempt((v) => v + 1);
      }
    } finally {
      setBusy(false);
    }
  };

  const runTurnOff = async (user: WorkspaceSkillUser) => {
    if (busy || !turnOff) return;
    setBusy(true);
    const out = await turnOff(name, user);
    setBusy(false);
    setConfirming(null);
    if (out.kind === "saved" || out.kind === "unchanged") {
      setNotice(null);
      // The row that opened the menu is gone: focus lands on the tab, not the page.
      setTimeout(() => document.getElementById("crm-tab-skill-used-by")?.focus(), 0);
    } else if (out.kind === "conflict") {
      setNotice({ text: t("Someone else changed {agent}’s profile, so nothing was saved.").replace("{agent}", () => user.name), user });
    } else if (out.kind === "unconfirmed") {
      setNotice({ text: t("Not confirmed: the connection dropped. Reload to see what the board holds."), user });
    } else if (out.kind === "refused") toast("err", t(out.message));
  };

  const saveDraft = async ({ files }: { name: string; files: Record<string, string> }) => {
    if (!deps.draft || !skill || busy) return;
    setBusy(true);
    setDraftError(null);
    try {
      const res = await deps.draft(name, files, skill.latest_revision);
      setEditing(false);
      onChanged?.(res.skill);
      setShown(undefined);
      focusNext.current = "approve";
      setAttempt((v) => v + 1);
      toast(
        "ok",
        t("Saved draft revision {n} of {name}.")
          .replace("{n}", String(res.revision.revision))
          .replace("{name}", () => name),
      );
    } catch (error) {
      const why = refusal(error);
      setDraftError(t(why.text));
      if (why.code === "revision_conflict") {
        setShown(undefined);
        setAttempt((v) => v + 1);
      }
    } finally {
      setBusy(false);
    }
  };

  const ready = read !== null && read !== "failed" ? read : null;
  const skill = ready?.skill;
  const approved = skill?.approved ?? null;
  const activeUsers = skill?.used_by ? usedByCounts(skill.used_by).active : null;
  const other = ready && skill ? otherRevision(skill, ready.revision.revision) : null;

  const tabs: DrawerTab[] = ready && skill
    ? [
        {
          id: "overview",
          label: t("Overview"),
          panel: (
            <OverviewPanel
              read={ready}
              profileId={profileId}
              onSeeAll={() => {
                setTab("used-by");
                setTimeout(() => document.getElementById("crm-tab-skill-used-by")?.focus(), 0);
              }}
              assign={
                assign
                  ? {
                      agentName,
                      draft: !approved,
                      on: assign.on,
                      canChange: assign.canChange && approved !== null,
                      pending: assign.pending,
                      onToggle: () => void assign.toggle(name, !(assign.on ?? false)),
                    }
                  : undefined
              }
            />
          ),
        },
        { id: "instructions", label: t("Instructions"), panel: <InstructionsPanel read={ready} /> },
        ...(skill.used_by !== undefined
          ? [
              {
                id: "used-by",
                label: `${t("Used by")} (${activeUsers})`,
                panel: (
                  <UsedByPanel
                    users={skill.used_by}
                    profileId={profileId}
                    writable={writable}
                    scopedRoster={scopedRoster}
                    notice={notice ? { text: notice.text, reload: () => { const user = notice.user; setNotice(null); reloadFor?.(user); } } : null}
                    onTurnOff={turnOff ? (user) => !busy && setConfirming({ turnOff: user }) : undefined}
                  />
                ),
              },
            ]
          : []),
        { id: "files", label: `${t("Files")} (${referenceFiles(ready.revision.files).length})`, panel: <FilesPanel files={ready.revision.files} /> },
      ]
    : [];

  const footer = ready && skill ? (
    writable
      ? {
          secondary: (
            <>
              {deps.draft !== undefined && (
                <Button size="sm" id="wskill-edit" onClick={() => { setDraftError(null); setEditing(true); }}>
                  {t("Edit draft")}
                </Button>
              )}
              {!approved && (
                <Button size="sm" variant="danger" onClick={() => !busy && setConfirming("delete")}>
                  {t("Delete")}
                </Button>
              )}
              {approved && (
                <Button size="sm" id="wskill-revoke" onClick={() => !busy && setConfirming("revoke")}>
                  {t("Revoke")}
                </Button>
              )}
            </>
          ),
          primary: !ready.approved ? (
            <Button
              size="sm"
              variant="primary"
              id="wskill-approve"
              onClick={() =>
                void decide(() => deps.approve(name, ready.revision.revision, ready.revision.digest), "Approved revision {n} of {name}.", ready.revision.revision, {
                  done: "revoke",
                  failed: "approve",
                })
              }
            >
              {t("Approve revision {n}").replace("{n}", String(ready.revision.revision))}
            </Button>
          ) : undefined,
          note: approved
            ? t("Approved revision {n} is what agents read.").replace("{n}", String(approved.revision))
            : t("Revision {n} isn’t approved yet.").replace("{n}", String(ready.revision.revision)),
        }
      : { note: t("Only the workspace owner can approve, revoke or delete skills.") }
    ) : {};

  return (
    <>
      <DrawerShell
        kind="skill"
        scrim="board"
        modal
        trapFocus
        size="md"
        label={name}
        title={name}
        subtitle={ready?.revision.description}
        pills={
          skill ? (
            <>
              <SkillPills skill={skill} />
              {other && (
                <span className="sk-rev">
                  {t(other.kind === "draft" ? "Showing approved revision" : "Showing draft revision")} ·{" "}
                  <button
                    type="button"
                    className="lnk"
                    onClick={() => {
                      focusNext.current = "top";
                      setShown(other.revision);
                    }}
                  >
                    {t(other.kind === "draft" ? "see draft" : "see approved")}
                  </button>
                </span>
              )}
            </>
          ) : undefined
        }
        tabs={tabs}
        tab={tab}
        onTab={setTab}
        preventClose={busy}
        edit={
          editing && ready && skill
            ? {
                formId: "wskill-edit-form",
                title: t("Edit draft"),
                saveLabel: t("Save draft"),
                pending: busy,
                onCancel: () => {
                  if (!busy) {
                    setEditing(false);
                    setDraftError(null);
                  }
                },
                body: (
                  <SkillDraftForm
                    formId="wskill-edit-form"
                    mode="edit"
                    initialName={name}
                    initialDescription={extractDescription(ready.revision.files["SKILL.md"] ?? "") || ready.revision.description}
                    initialBody={stripFrontmatter(ready.revision.files["SKILL.md"] ?? "")}
                    initialReferences={editableReferences(ready.revision.files)}
                    busy={busy}
                    serverError={draftError}
                    onSubmit={(payload) => void saveDraft(payload)}
                  />
                ),
              }
            : null
        }
        state={
          read === null ? (
            <Note>{t("Loading…")}</Note>
          ) : read === "failed" ? (
            <Failed
              text="This skill couldn’t be read."
              onRetry={() => {
                focusNext.current = "top";
                setAttempt((v) => v + 1);
              }}
            />
          ) : undefined
        }
        {...footer}
        onClose={onClose}
      />
      {/* Outside the drawer: its transform would pin a fixed dialog inside it. */}
      {confirming === "delete" && ready && (
        <ConfirmDialog
          title={t("Delete {name}?").replace("{name}", () => name)}
          body={t("This removes the skill and every revision of it. It is not approved, so no agent reads it.")}
          confirmLabel={t("Delete")}
          pending={busy}
          error={null}
          onConfirm={() =>
            void decide(async () => {
              await deps.remove(name, ready.skill.latest_revision);
              return null;
            }, "Deleted {name}.", 0, { done: null, failed: null })
          }
          onCancel={() => setConfirming(null)}
        />
      )}
      {confirming === "revoke" && approved && (
        <ConfirmDialog
          title={t("Revoke {name}?").replace("{name}", () => name)}
          body={
            activeUsers === null
              ? t("The approved revision is withdrawn. Every agent that lists this skill loses it at its next read. The skill and its revisions stay, so you can approve it again.")
              : t(activeUsers === 1 ? "The approved revision is withdrawn. {n} active agent loses this skill at its next read. The skill and its revisions stay, so you can approve it again." : "The approved revision is withdrawn. {n} active agents lose this skill at their next read. The skill and its revisions stay, so you can approve it again.").replace("{n}", String(activeUsers))
          }
          confirmLabel={t("Revoke")}
          pending={busy}
          error={null}
          onConfirm={() => void decide(() => deps.revoke(name, approved.digest), "Revoked {name}.", 0, { done: "approve", failed: "revoke" })}
          onCancel={() => setConfirming(null)}
        />
      )}
      {typeof confirming === "object" && confirming !== null && turnOff && (
        <ConfirmDialog
          title={t("Turn off {skill} for {agent}?").replace("{skill}", () => name).replace("{agent}", () => confirming.turnOff.name)}
          body={
            <>
              <p>{t("{agent} stops seeing this skill in new work and can no longer read it. Other agents keep it.").replace("{agent}", () => confirming.turnOff.name)}</p>
              <p>{t("This changes the agent’s profile, not the skill. You can turn it back on from the agent’s Skills tab.")}</p>
            </>
          }
          confirmLabel={t("Turn off")}
          pending={busy}
          error={null}
          onConfirm={() => void runTurnOff(confirming.turnOff)}
          onCancel={() => setConfirming(null)}
        />
      )}
    </>
  );
}
