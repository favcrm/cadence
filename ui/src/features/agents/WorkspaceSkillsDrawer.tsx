import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import type { WorkspaceSkillSummary } from "../../lib/types";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { SkillDrawer, SkillPills } from "./SkillDrawer";
import { SkillDraftForm } from "./SkillPanels";
import { libraryAbsent, liveSkillDeps, refusal, usedByNote, type SkillDeps } from "./workspaceSkills";

type LibraryState =
  | { state: "reading" }
  | { state: "failed" }
  | { state: "absent" }
  | { state: "ready"; skills: WorkspaceSkillSummary[] };

function isPendingDraft(skill: WorkspaceSkillSummary): boolean {
  return !skill.approved || skill.latest_revision > skill.approved.revision;
}

function sortWorkspaceSkills(skills: readonly WorkspaceSkillSummary[]): WorkspaceSkillSummary[] {
  return [...skills].sort((a, b) => {
    const draftDiff = Number(isPendingDraft(b)) - Number(isPendingDraft(a));
    return draftDiff !== 0 ? draftDiff : a.name.localeCompare(b.name);
  });
}

/**
 * Workspace-level skill library drawer: reachable from the Agents directory
 * and Home `/skills`, opening a single SkillDrawer cleanly when a skill is picked.
 */
export function WorkspaceSkillsDrawer({
  writable,
  scopedRoster = false,
  deps = liveSkillDeps,
  toast,
  onSkillChanged,
  onSkillGone,
  onClose,
}: {
  writable: boolean;
  scopedRoster?: boolean;
  deps?: SkillDeps;
  toast: (kind: ToastMsg["kind"], text: string) => void;
  onSkillChanged?: (skill: WorkspaceSkillSummary) => void;
  onSkillGone?: (name: string) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const [library, setLibrary] = useState<LibraryState>({ state: "reading" });
  const [attempt, setAttempt] = useState(0);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<{ name: string; tab: "overview" | "instructions" } | null>(null);
  const [creating, setCreating] = useState(false);
  const [createBusy, setCreateBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const returnFocusTo = useRef<HTMLElement | null>(null);

  useEffect(() => {
    let live = true;
    setLibrary((now) => (now.state === "ready" ? now : { state: "reading" }));
    deps
      .list()
      .then((skills) => {
        if (live) setLibrary({ state: "ready", skills });
      })
      .catch((error: unknown) => {
        if (live) setLibrary({ state: libraryAbsent(error) ? "absent" : "failed" });
      });
    return () => {
      live = false;
    };
  }, [deps, attempt]);

  const replace = (skill: WorkspaceSkillSummary) =>
    setLibrary((now) => {
      if (now.state !== "ready") return now;
      const found = now.skills.some((s) => s.name === skill.name);
      const skills = found
        ? now.skills.map((s) => (s.name === skill.name ? { ...skill, used_by: skill.used_by ?? s.used_by } : s))
        : [...now.skills, skill];
      return { state: "ready", skills };
    });

  const drop = (name: string) =>
    setLibrary((now) => (now.state === "ready" ? { state: "ready", skills: now.skills.filter((s) => s.name !== name) } : now));

  const createDraft = async ({ name, files }: { name: string; files: Record<string, string> }) => {
    if (!deps.draft || createBusy) return;
    setCreateBusy(true);
    setCreateError(null);
    try {
      const res = await deps.draft(name, files, 0);
      replace(res.skill);
      onSkillChanged?.(res.skill);
      setCreating(false);
      setAttempt((n) => n + 1);
      toast(
        "ok",
        t("Saved draft revision {n} of {name}.")
          .replace("{n}", String(res.revision.revision))
          .replace("{name}", () => res.skill.name),
      );
      setOpen({ name: res.skill.name, tab: "instructions" });
    } catch (error) {
      const why = refusal(error);
      setCreateError(t(why.text));
    } finally {
      setCreateBusy(false);
    }
  };

  if (open !== null) {
    return (
      <SkillDrawer
        key={open.name}
        name={open.name}
        initialTab={open.tab}
        writable={writable}
        scopedRoster={scopedRoster}
        deps={deps}
        toast={toast}
        onChanged={(skill) => {
          replace(skill);
          onSkillChanged?.(skill);
        }}
        onGone={(name) => {
          drop(name);
          onSkillGone?.(name);
          setOpen(null);
        }}
        onClose={() => {
          setOpen(null);
          const opener = returnFocusTo.current;
          returnFocusTo.current = null;
          setTimeout(() => {
            if (opener?.isConnected) opener.focus();
          }, 0);
        }}
      />
    );
  }

  const skills = library.state === "ready" ? sortWorkspaceSkills(library.skills) : [];
  const term = query.trim().toLowerCase();
  const shown = term === ""
    ? skills
    : skills.filter((s) => s.name.toLowerCase().includes(term) || s.description.toLowerCase().includes(term));

  const usedBy = (skill: WorkspaceSkillSummary) => {
    const note = usedByNote(skill);
    return note ? t(note.text).replace("{n}", String(note.n)) : "";
  };

  return (
    <DrawerShell
      kind="workspace-skills"
      scrim="board"
      modal
      trapFocus
      size="md"
      label={t("Workspace skills")}
      title={t("Workspace skills")}
      preventClose={createBusy}
      onClose={onClose}
      primary={
        writable && deps.draft !== undefined && library.state === "ready" ? (
          <Button size="sm" variant="primary" onClick={() => { setCreateError(null); setCreating(true); }}>
            {t("New skill")}
          </Button>
        ) : undefined
      }
      edit={
        creating
          ? {
              formId: "wskill-lib-create-form",
              title: t("Save a draft package for review"),
              saveLabel: t("Save draft"),
              pending: createBusy,
              onCancel: () => {
                if (!createBusy) setCreating(false);
              },
              body: (
                <SkillDraftForm
                  formId="wskill-lib-create-form"
                  mode="create"
                  busy={createBusy}
                  serverError={createError}
                  onSubmit={(payload) => void createDraft(payload)}
                />
              ),
            }
          : null
      }
      state={
        library.state === "reading" ? (
          <p className="rc-note">{t("Loading…")}</p>
        ) : library.state === "failed" ? (
          <p className="rc-note" role="alert">
            {t("Workspace skills couldn’t be read.")}{" "}
            <button type="button" className="lnk" onClick={() => setAttempt((n) => n + 1)}>
              {t("Retry")}
            </button>
          </p>
        ) : library.state === "absent" ? (
          <p className="rc-note">{t("Workspace skills aren’t available on this board.")}</p>
        ) : undefined
      }
    >
      {library.state === "ready" && (
        <div className="ap-body" data-work-panel="workspace-skills-drawer">
          {skills.length > 0 && (
            <div className="sk-tools">
              <input
                className="field sk-search"
                type="search"
                value={query}
                placeholder={t("Search workspace skills")}
                aria-label={t("Search workspace skills")}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>
          )}
          {skills.length === 0 ? (
            <p className="rc-note">{t("No workspace skills yet. Agents draft them; an owner approves them.")}</p>
          ) : shown.length === 0 ? (
            <p className="rc-note">{t("No matching skills.")}</p>
          ) : (
            <div className="sk-list">
              {shown.map((skill) => (
                <div key={skill.name} className="sk-card">
                  <button
                    type="button"
                    className="sk-open"
                    data-workspace-skill={skill.name}
                    onClick={(e) => {
                      returnFocusTo.current = e.currentTarget;
                      setOpen({ name: skill.name, tab: isPendingDraft(skill) ? "instructions" : "overview" });
                    }}
                  >
                    <span className="sk-title">
                      <b>{skill.name}</b>
                      <SkillPills skill={skill} />
                    </span>
                    <span className="sk-desc">{skill.description}</span>
                    {usedBy(skill) && <span className="sk-meta">{usedBy(skill)}</span>}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </DrawerShell>
  );
}
