import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import { useLocale } from "../../lib/locale";
import type { ProjectPolicy } from "../../lib/types";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";

/** The recorded spelling, which is also the CLI flag's spelling. */
type Visibility = ProjectPolicy["visibility"];

interface Draft {
  visibility: Visibility;
  members: string[];
}


/** The CLI command for exactly this state.
 *
 *  `--grant` is the full list and omitting it clears, so grants are always
 *  spelled out — including on a workspace-visible project, where they are
 *  stored but inert. Printing them is what keeps this line and the save
 *  button describing one state instead of two. */
function cliCommand(project: string, draft: Draft): string {
  const grants = draft.members.map((member) => ` --grant ${member}`).join("");
  return `cadence issue project policy ${project} --visibility ${draft.visibility.replace("_", "-")}${grants}`;
}

/**
 * A project's access policy (CAD-1950). The values are the tracker's own:
 * `workspace-visible` / `private`, grants `users/<handle>` or `operator`.
 *
 * The board is a second entry point, not a second writer — Save posts to the
 * same `set_project_policy` the CLI calls, so the version bump, the grant
 * grammar and the isolation gate are one implementation. Everything here is
 * a view over that record: an absent policy is called absent, never rendered
 * as workspace-visible, because those are different records.
 */
export default function ProjectSettings({
  project,
  block,
  onToast,
}: {
  project: string;
  /** Why this session may not change the policy, or null when it may. */
  block: string | null;
  onToast: (text: string) => void;
}) {
  const { t } = useLocale();
  const resource = resources.projectPolicy(project);
  const state = useQuery(resource);
  const saved = state.data ?? null;
  const [edits, setEdits] = useState<Draft | null>(null);
  const [handle, setHandle] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A new payload is a new baseline: pending edits belong to the revision
  // they were made against, not to whatever landed afterwards.
  const version = saved?.policy?.version ?? 0;
  useEffect(() => {
    setEdits(null);
    setError(null);
  }, [project, version]);

  const base: Draft | null = saved
    ? {
        visibility: saved.policy?.visibility ?? "workspace_visible",
        members: saved.policy?.members ?? [],
      }
    : null;
  const draft = edits ?? base;
  const dirty =
    draft !== null && base !== null
      ? draft.visibility !== base.visibility ||
        draft.members.length !== base.members.length ||
        draft.members.some((member, index) => member !== base.members[index])
      : false;
  const locked = block !== null;
  const privateOffered = saved?.private_available !== false;
  const pendingPrivate = draft?.visibility === "private" && !privateOffered;

  function edit(next: Partial<Draft>) {
    if (!draft || locked) return;
    setEdits({ ...draft, ...next });
  }

  function addGrant() {
    const value = handle.trim();
    if (!draft || !value || locked) return;
    if (draft.members.includes(value)) {
      setError(t("That grant is already on the list."));
      return;
    }
    setError(null);
    setHandle("");
    edit({ members: [...draft.members, value] });
  }

  async function save() {
    if (!draft || locked || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.setProjectPolicy(project, {
        visibility: draft.visibility.replace("_", "-") as "workspace-visible" | "private",
        members: draft.members,
      });
      setEdits(null);
      onToast(t("Project policy saved."));
      void resource.refresh();
    } catch (e) {
      // The server's sentence names the refused subject or the gate; it is
      // more precise than anything this screen could say.
      setError(e instanceof ApiError ? e.message : t("The policy could not be saved."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="project-overview px-4 lg:px-8 py-6 min-w-0">
      <header className="project-page-heading">
        <div>
          <div className="slabel">{t("Project settings")}</div>
          <h1>{project}</h1>
          <p>{t("Who can see this project, and who can open it.")}</p>
        </div>
      </header>
      <ResourceGate
        state={state}
        loading={t("Reading the project policy…")}
        failed={t("The project policy could not be loaded")}
        onRetry={() => void resource.refresh()}
      />
      <StaleChip state={state} />
      {saved && draft && (
        <>
          {!saved.recorded && (
            <p className="roadmap-notice">
              {t(
                "No policy recorded — every caller can see this project. Saving one records the choice below.",
              )}
            </p>
          )}
          <section className="card max-w-2xl p-5" aria-label={t("Visibility")}>
            <div className="slabel">{t("Visibility")}</div>
            <div className="mt-3 grid gap-2">
              {(["workspace_visible", "private"] as Visibility[]).map((option) => {
                const on = draft.visibility === option;
                const off = locked || (option === "private" && !privateOffered);
                return (
                  <button
                    key={option}
                    type="button"
                    onClick={() => edit({ visibility: option })}
                    disabled={off}
                    aria-pressed={on}
                    className={`w-full rounded border p-3 text-left transition-colors ${
                      on ? "border-accent bg-accent/5" : "border-ink-700"
                    } ${off ? "opacity-60" : ""}`}
                  >
                    <span className="text-secondary text-ink-100 font-medium">
                      {t(option === "private" ? "Private" : "Workspace-visible")}
                    </span>
                    <span className="mt-1 block text-label text-ink-400">
                      {t(
                        option === "private"
                          ? "Only the people granted below can see it. An owner can still open it without a grant."
                          : "Every current and future member of this workspace can see it. Grants are stored but admit no one.",
                      )}
                      {option === "private" && !privateOffered && (
                        <>
                          {" "}
                          {t(
                            "This build cannot record private — the isolation gate has not passed.",
                          )}
                          {base?.visibility === "private" &&
                            ` ${t("This project already carries a private policy recorded earlier.")}`}
                        </>
                      )}
                    </span>
                  </button>
                );
              })}
            </div>

            {draft.visibility === "private" && (
              <div className="mt-5">
                <div className="slabel">{t("Grants")}</div>
                <ul className="mt-2 grid gap-1">
                  {draft.members.map((member) => (
                    <li key={member} className="flex items-center gap-2 text-secondary text-ink-200">
                      <span className="num text-micro text-ink-300 break-all">{member}</span>
                      {!locked && (
                        <button
                          type="button"
                          className="btn btn-ghost ml-auto"
                          onClick={() =>
                            edit({ members: draft.members.filter((item) => item !== member) })
                          }
                        >
                          {t("Remove")}
                        </button>
                      )}
                    </li>
                  ))}
                  {draft.members.length === 0 && (
                    <li className="text-label text-ink-500">
                      {t("No grants yet — an owner can still open it.")}
                    </li>
                  )}
                </ul>
                {!locked && (
                  <div className="mt-2 flex flex-wrap gap-2">
                    <input
                      className="field min-w-0 flex-1"
                      value={handle}
                      onChange={(event) => setHandle(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          addGrant();
                        }
                      }}
                      placeholder="users/alex"
                      aria-label={t("Grant a caller")}
                      spellCheck={false}
                    />
                    <button type="button" className="btn" onClick={addGrant} disabled={!handle.trim()}>
                      {t("Add")}
                    </button>
                  </div>
                )}
                <p className="mt-2 text-label text-ink-500">
                  {t(
                    "Accepts operator or users/<handle>. Agents and services cannot be granted access.",
                  )}
                </p>
              </div>
            )}

            {draft.visibility !== "private" && draft.members.length > 0 && (
              <p className="mt-4 text-label text-warn">
                {t(
                  "This project has stored grants. They admit no one while it is workspace-visible; they apply if it becomes private.",
                )}
              </p>
            )}

            {pendingPrivate && dirty && !locked && (
              <p className="mt-4 text-label text-warn">
                {t(
                  "These changes would record private, which this build refuses — the isolation gate has not passed. Save stays off rather than sending a write the tracker would reject.",
                )}
              </p>
            )}
            {block && <p className="mt-4 text-label text-ink-400">{block}</p>}
            {error && (
              <p className="mt-3 text-secondary text-fail" role="alert">
                {error}
              </p>
            )}
            {!locked && (
              <div className="mt-4 flex items-center gap-2">
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={() => void save()}
                  disabled={!dirty || busy || pendingPrivate}
                >
                  {busy ? t("Saving…") : t("Save policy")}
                </button>
                {dirty && !busy && (
                  <button type="button" className="btn btn-ghost" onClick={() => setEdits(null)}>
                    {t("Discard")}
                  </button>
                )}
              </div>
            )}
          </section>
          <section className="card mt-4 max-w-2xl p-5" aria-label={t("Same change from the CLI")}>
            <div className="slabel">{t("Same change from the CLI")}</div>
            <pre className="mt-2 overflow-x-auto break-words whitespace-pre-wrap rounded border border-ink-700 bg-ink-900 p-3 font-mono text-label text-ink-200">
              {cliCommand(project, draft)}
            </pre>
            <p className="mt-2 text-label text-ink-500">
              {t("--grant is the full list; leaving it out clears every grant.")}
            </p>
          </section>
        </>
      )}
    </main>
  );
}
