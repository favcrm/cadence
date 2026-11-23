import { useEffect, useId, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { ApiError } from "../../lib/apiError";
import { privateSessionGeneration } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import { useLocale } from "../../lib/locale";
import { readTeamMembers, type TeamMember } from "../../lib/teamRoster";
import { objectField } from "../../lib/wire";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import Select from "../../ui/Select";
import { cliCommand, type PolicyDraft } from "./policyView";
import {
  draftDirty,
  draftOf,
  nativeGrant,
  policyWrite,
  privateRefused,
  readPolicy,
  type PolicyView,
} from "./policyContract";

type Draft = PolicyDraft;

/**
 * A project's access policy (CAD-1950, CAD-2255).
 *
 * Two stores answer this page and they are not interchangeable: a native board
 * records `{visibility, members}` in `project.yaml`, while a hosted board keeps
 * `{visibility, grants: <member subject ids>, revision}` and refuses a new
 * private row until end-to-end isolation is proven. `policyContract` reads and
 * writes both; this screen only renders what that module proved. An absent
 * record is called absent, an unreadable answer is an error, and no capability
 * is defaulted on — a private transition that cannot be proven stays disabled.
 */
export default function ProjectSettings({
  project,
  block,
  hosted,
  onToast,
}: {
  project: string;
  /** Why this session may not change the policy, or null when it may. */
  block: string | null;
  /** The board's own claim from `/api/meta`; null when it has not answered. */
  hosted: boolean | null;
  onToast: (text: string) => void;
}) {
  const { t } = useLocale();
  const resource = resources.projectPolicy(project);
  const state = useQuery(resource);
  const [edits, setEdits] = useState<Draft | null>(null);
  const [handle, setHandle] = useState("");
  const [picked, setPicked] = useState("");
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [roster, setRoster] = useState<TeamMember[] | null>(null);
  const [rosterError, setRosterError] = useState<string | null>(null);
  const [rosterAttempt, setRosterAttempt] = useState(0);
  const selectId = useId();

  const view: PolicyView = useMemo(
    () => readPolicy(state.data, hosted),
    [state.data, hosted],
  );
  const version = view.kind === "recorded" ? view.revision : -1;

  // A new payload is a new baseline: pending edits belong to the revision they
  // were made against, not to whatever landed afterwards. The error stays —
  // it is what explains why the baseline moved under a refused save.
  useEffect(() => {
    setEdits(null);
  }, [project, version, view.kind]);
  useEffect(() => {
    setError(null);
    setHandle("");
    setPicked("");
    setAdding(false);
    setBusy(false);
  }, [project]);

  // The hosted roster is the only source of verified member subject ids.
  useEffect(() => {
    if (hosted !== true) {
      setRoster(null);
      setRosterError(null);
      return;
    }
    const abort = new AbortController();
    setRosterError(null);
    readTeamMembers(abort.signal)
      .then((rows) => {
        if (abort.signal.aborted) return;
        setRoster(rows);
        setRosterError(null);
      })
      .catch((e: unknown) => {
        if (abort.signal.aborted) return;
        setRoster(null);
        setRosterError(e instanceof Error ? e.message : "the member roster could not be read");
      });
    return () => abort.abort();
  }, [hosted, project, rosterAttempt]);

  // While the store has no answer yet, ResourceGate owns the screen: an
  // "unreadable" judgement here would blame the board for a request in flight.
  const hasAnswer = state.data !== null;
  const base = draftOf(view);
  const draft = edits ?? base;
  const dirty = draftDirty(view, draft);
  const locked = block !== null;
  const unreadable = view.kind === "unreadable";
  const privateOffered =
    view.kind === "recorded"
      ? view.privateOffered === true
      : view.kind === "absent"
        ? view.privateOffered === true
        : false;
  const pendingPrivate = privateRefused(view, draft, hosted === true);
  const hostedRosterKnown = roster !== null;
  const grantOptions = (roster ?? [])
    .filter((row) => !(draft?.grants ?? []).includes(row.userId))
    .map((row) => ({
      value: row.userId,
      label: row.name.trim() || row.email || row.userId,
      hint: row.name.trim() && row.email ? row.email : undefined,
    }));

  function edit(next: Partial<Draft>) {
    if (!draft || adding || busy || locked) return;
    setEdits({ ...draft, ...next });
  }

  const editingLocked = adding || busy || locked;

  // The last rendered draft, so a grant resolved by a roster read can prove
  // the list it is about to change is still the list on screen.
  const draftRef = useRef<Draft | null>(null);
  draftRef.current = draft;
  const projectRef = useRef({ key: project, epoch: 0 });
  if (projectRef.current.key !== project)
    projectRef.current = { key: project, epoch: projectRef.current.epoch + 1 };
  useEffect(() => () => { projectRef.current.epoch += 1; }, []);

  async function addGrant() {
    if (!draft || locked || adding || busy) return;
    if (hosted === true) {
      const id = picked.trim();
      if (!id) return;
      const basis = JSON.stringify(draft);
      const epoch = projectRef.current.epoch;
      const generation = privateSessionGeneration();
      const current = () => projectRef.current.epoch === epoch && privateSessionGeneration() === generation;
      setAdding(true);
      setError(null);
      try {
        // Membership is re-proved now, not taken from the mount: a removed
        // member's id must not be written into the policy by a stale page.
        const fresh = await readTeamMembers();
        if (!current()) return;
        setRoster(fresh);
        setRosterError(null);
        if (!fresh.some((row) => row.userId === id)) {
          setError(t("That id is not a member of this workspace. Nothing was added."));
          return;
        }
        if (JSON.stringify(draftRef.current) !== basis) {
          setError(t("The policy changed while the member list was re-read. Nothing was added — review the list again."));
          return;
        }
        setPicked("");
        edit({ grants: [...draft.grants, id] });
      } catch (e) {
        if (!current()) return;
        setRoster(null);
        setRosterError(e instanceof Error ? e.message : "the member roster could not be read");
        setError(t("The member roster could not be re-read, so the grant was not added."));
      } finally {
        if (current()) setAdding(false);
      }
      return;
    }
    const value = nativeGrant(handle);
    if (!value) {
      setError(t("A native grant is `operator` or `users/<handle>`. Nothing was added."));
      return;
    }
    if (draft.grants.includes(value)) {
      setError(t("That grant is already on the list."));
      return;
    }
    setError(null);
    setHandle("");
    edit({ grants: [...draft.grants, value] });
  }

  async function save() {
    if (!draft || locked || adding || busy || unreadable || hosted === null) return;
    const write = policyWrite(view, draft, hosted);
    if (!write) {
      setError(t("The policy cannot be saved from this read — reload the page and try again."));
      return;
    }
    if (privateRefused(view, draft, hosted)) {
      setError(
        hosted
          ? t("This board refuses a new private policy while end-to-end isolation is unproven. Nothing was saved.")
          : t("This build refuses private until the isolation gate has passed. Nothing was saved."),
      );
      return;
    }
    const epoch = projectRef.current.epoch;
    const generation = privateSessionGeneration();
    const current = () => projectRef.current.epoch === epoch && privateSessionGeneration() === generation;
    setBusy(true);
    setError(null);
    try {
      const wrote =
        write.contract === "native"
          ? await api.setProjectPolicyNative(project, {
              visibility: write.visibility,
              members: write.members,
            })
          : await api.setProjectPolicyHosted(project, {
              visibility: write.visibility,
              grants: write.grants,
              if_rev: write.if_rev,
            });
      if (!current()) return;
      // A write's answer is its receipt. Anything this reader cannot prove is
      // not a save: the page keeps saying the record is unread and refreshes.
      const receipt = objectField(wrote);
      const raw =
        write.contract === "native"
          ? {
              // The response's `policy` is the committed record; the gate
              // capability is the one this read already carried.
              recorded: true,
              policy: receipt && "policy" in receipt ? receipt.policy : null,
              private_available: view.kind === "recorded" || view.kind === "absent" ? view.privateOffered : false,
            }
          : wrote;
      const committed = readPolicy(raw, write.contract === "hosted");
      if (
        committed.kind !== "recorded" ||
        committed.visibility !== draft.visibility ||
        committed.grants.length !== draft.grants.length ||
        committed.grants.some((grant) => !draft.grants.includes(grant))
      ) {
        throw new Error("The board answered without a matching policy receipt.");
      }
      resource.write(() => raw);
      setEdits(null);
      onToast(
        write.contract === "hosted"
          ? t("Project policy saved. Stored grants still admit no one unless it is private.")
          : t("Project policy saved."),
      );
      void resource.refresh();
    } catch (e) {
      if (!current()) return;
      const detail =
        e instanceof ApiError
          ? e.code === "revision_conflict"
            ? t("The policy changed since this page read it. Nothing was saved — the current record is being reloaded.")
            : e.code === "private_unavailable"
              ? t("This board refuses a new private policy while end-to-end isolation is unproven. Nothing was saved.")
              : e.message
          : e instanceof Error
            ? e.message
            : t("The policy could not be saved.");
      setError(detail);
      void resource.refresh();
    } finally {
      if (current()) setBusy(false);
    }
  }

  const sharesHeading = hosted === true ? t("Hosted project access") : t("Project access");

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

      {!hasAnswer ? null : unreadable ? (
        <section className="card max-w-2xl p-5" role="alert">
          <div className="slabel">{sharesHeading}</div>
          <p className="mt-2 text-secondary text-fail">
            {t("This board answered with a policy shape this page cannot prove, so no visibility or grant is shown or offered.")}
            {hosted === null && ` ${t("It also did not say whether it is hosted.")}`}
          </p>
          <p className="mt-2 text-label text-ink-500">
            {view.kind === "unreadable" ? view.reason : ""}
          </p>
          <button type="button" className="btn mt-3" onClick={() => void resource.refresh()}>
            {t("Read again")}
          </button>
        </section>
      ) : (
        <>
          {view.kind === "absent" && (
            <p className="roadmap-notice">
              {t("No policy recorded — every caller can see this project. Saving one records the choice below.")}
            </p>
          )}
          {view.kind === "not_stored" && (
            <p className="roadmap-notice">
              {t("This workspace has no stored policy for this project, so its access is not recorded as workspace-visible. Saving writes the first policy and pins its revision.")}
            </p>
          )}
          {draft && (
            <>
              <section className="card max-w-2xl p-5" aria-label={t("Visibility")}>
                <div className="slabel">{t("Visibility")}</div>
                <div className="mt-3 grid gap-2">
                  {(["workspace_visible", "private"] as const).map((option) => {
                    const on = draft.visibility === option;
                    const canChoosePrivate = privateOffered;
                    const off = editingLocked;
                    return (
                      <button
                        key={option}
                        type="button"
                        onClick={() => edit({ visibility: option })}
                        disabled={off}
                        aria-pressed={on}
                        className={`ps-visibility w-full rounded border p-3 text-left transition-colors ${
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
                          {option === "private" && !canChoosePrivate && (
                            <>
                              {" "}
                              {hosted === true
                                ? t("This workspace refuses a new private policy until end-to-end isolation is proven.")
                                : t("This build cannot record private — the isolation gate has not passed.")}
                              {view.kind === "recorded" &&
                                view.visibility === "private" &&
                                ` ${t("This project already carries a private policy recorded earlier.")}`}
                            </>
                          )}
                        </span>
                      </button>
                    );
                  })}
                </div>

                {(draft.visibility === "private" || draft.grants.length > 0) && (
                  <div className="mt-5">
                    <div className="slabel">{t("Grants")}</div>
                    <ul className="mt-2 grid gap-1">
                      {draft.grants.map((grant) => {
                        const member =
                          hosted === true
                            ? (roster?.find((row) => row.userId === grant) ?? null)
                            : null;
                        return (
                          <li key={grant} className="flex items-center gap-2 text-secondary text-ink-200">
                            {member ? (
                              <span className="min-w-0">
                                <span className="block truncate">{member.name.trim() || member.email || grant}</span>
                                <span className="num text-micro text-ink-500 break-all">{grant}</span>
                              </span>
                            ) : (
                              <span className="num text-micro text-ink-300 break-all">{grant}</span>
                            )}
                            {hosted === true && hostedRosterKnown && !member && (
                              <span className="chip bg-warn/10 text-warn">{t("not a current member")}</span>
                            )}
                            {!locked && (
                              <button
                                type="button"
                                className="btn btn-ghost ml-auto"
                                disabled={editingLocked}
                                onClick={() =>
                                  edit({ grants: draft.grants.filter((item) => item !== grant) })
                                }
                              >
                                {t("Remove")}
                              </button>
                            )}
                          </li>
                        );
                      })}
                      {draft.grants.length === 0 && draft.visibility === "private" && (
                        <li className="text-label text-ink-500">
                          {t("No grants yet — an owner can still open it.")}
                        </li>
                      )}
                    </ul>

                    {!locked && (
                      <div className="ps-grant-add mt-2">
                        {hosted === true ? (
                          <>
                            <Select
                              id={selectId}
                              className="ps-grant-field"
                              full
                              value={picked}
                              onChange={setPicked}
                              options={grantOptions}
                              placeholder={
                                rosterError
                                  ? t("Member roster unavailable")
                                  : grantOptions.length === 0
                                    ? t("Every member already has a grant")
                                    : t("Choose a member")
                              }
                              disabled={editingLocked || !hostedRosterKnown || grantOptions.length === 0}
                              aria-label={t("Grant a workspace member")}
                            />
                            <button
                              type="button"
                              className="btn"
                              onClick={() => void addGrant()}
                              disabled={!picked || !hostedRosterKnown || editingLocked}
                            >
                              {adding ? t("Adding…") : t("Add")}
                            </button>
                          </>
                        ) : (
                          <>
                            <input
                              className="field ps-grant-field min-w-0"
                              value={handle}
                              disabled={editingLocked}
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
                            <button type="button" className="btn" onClick={addGrant} disabled={!handle.trim() || editingLocked}>
                              {t("Add")}
                            </button>
                          </>
                        )}
                      </div>
                    )}
                    <p className="mt-2 text-label text-ink-500">
                      {hosted === true
                        ? t(
                            "Hosted grants are verified member ids. The member roster is re-read before a grant is added.",
                          )
                        : t(
                            "Accepts operator or users/<handle>. Agents and services cannot be granted access.",
                          )}
                    </p>
                    {hosted === true && rosterError && (
                      <p className="mt-1 flex flex-wrap items-center gap-2 text-label text-warn" role="status">
                        <span>
                          {t("The member roster could not be read, so no grant can be resolved to a person. Nothing was added.")}
                        </span>
                        <button
                          type="button"
                          className="btn btn-sm"
                          onClick={() => setRosterAttempt((count) => count + 1)}
                        >
                          {t("Retry roster")}
                        </button>
                      </p>
                    )}
                  </div>
                )}

                {draft.visibility !== "private" && draft.grants.length > 0 && (
                  <p className="mt-4 text-label text-warn">
                    {t(
                      "This project has stored grants. They admit no one while it is workspace-visible; they apply if it becomes private.",
                    )}
                  </p>
                )}

                {view.kind === "recorded" &&
                  view.visibility === "private" &&
                  draft.visibility !== "private" && (
                    <p className="mt-4 text-label text-warn">
                      {t(
                        "This project is private today. Saving workspace-visible lets every member of this workspace see it, and its stored grants stop admitting anyone.",
                      )}
                    </p>
                  )}

                {pendingPrivate && dirty && !locked && (
                  <p className="mt-4 text-label text-warn">
                    {hosted === true
                      ? t("These changes would record a new private policy, which this workspace refuses until isolation is proven. Save stays off rather than sending a write the server would reject.")
                      : t("These changes would record private, which this build refuses — the isolation gate has not passed. Save stays off rather than sending a write the tracker would reject.")}
                  </p>
                )}
                {block && <p className="mt-4 text-label text-ink-400">{block}</p>}
                {error && (
                  <p className="mt-3 text-secondary text-fail" role="alert">
                    {error}
                  </p>
                )}
                {!locked && (
                  <div className="mt-4 flex flex-wrap items-center gap-2">
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={() => void save()}
                      disabled={!dirty || adding || busy || pendingPrivate || hosted === null}
                    >
                      {busy ? t("Saving…") : t("Save policy")}
                    </button>
                    {dirty && !busy && (
                      <button type="button" className="btn btn-ghost" onClick={() => setEdits(null)} disabled={adding}>
                        {t("Discard")}
                      </button>
                    )}
                  </div>
                )}
              </section>

              {hosted === false && (
                <section className="card mt-4 max-w-2xl p-5" aria-label={t("Same change from the CLI")}>
                  <div className="slabel">{t("Same change from the CLI")}</div>
                  <pre className="mt-2 overflow-x-auto break-words whitespace-pre-wrap rounded border border-ink-700 bg-ink-900 p-3 font-mono text-label text-ink-200">
                    {cliCommand(project, draft)}
                  </pre>
                  <p className="mt-2 text-label text-ink-500">
                    {t("--grant is the full list; leaving it out clears every grant.")}
                  </p>
                </section>
              )}
            </>
          )}

        </>
      )}
    </main>
  );
}
