import { useCallback, useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import {
  PILOT_DESTINATION,
  PUBLISH_TIMEZONE,
  canCancel,
  dueLabel,
  importMatchesReviewed,
  isGrantIdUsable,
  mintApprovalId,
  parseDueEpoch,
  publishStateText,
  publishStateTone,
  randomHex,
  reconcileReading,
  refusalCopy,
  refusalFromError,
  showsUncertainReading,
  socialPublish,
  type PublishIntent,
  type PublishState,
} from "./socialPublish";
import { workspaceApps } from "./workspaceApps";

export interface PublishCandidate {
  run_id: string;
  effect_id: string;
  artifact_id: string;
  bundle_digest: string;
  title: string;
  caption: string;
  image_digest: string | null;
  writer: string;
  reviewer: string;
}

const message = (error: unknown) =>
  error instanceof Error
    ? error.message
    : "Could not complete this action. Refresh and try again.";

/** CAD-787 product panel: the accepted preview flow bound to the
 *  CAD-771/AOS-94 exact-destination states. The operator enters the
 *  AgenticOS `dpq_` send grant, chooses Post now (`due_epoch` at now) or
 *  Schedule (a future `due_epoch` with timezone), then confirms the exact
 *  caption, image, due time, timezone and destination (CAD-1027). Each
 *  confirmation mints one approval id that authorizes one intent. Digests
 *  always derive server-side from the approved run — never invented here. */
export default function PublishPanel({
  installId,
  contextId,
  candidates,
  canWrite,
  client = socialPublish,
  loadArtifact = workspaceApps.artifact,
  imageReceipts = workspaceApps.imageResults,
}: {
  installId: string;
  contextId: string | null;
  candidates: PublishCandidate[];
  canWrite: boolean;
  client?: typeof socialPublish;
  loadArtifact?: typeof workspaceApps.artifact;
  imageReceipts?: typeof workspaceApps.imageResults;
}) {
  const [intents, setIntents] = useState<PublishIntent[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [grantId, setGrantId] = useState("");
  // One operator confirmation: its approval id doubles as the request id,
  // so a retry or double submit of this confirmation replays one intent.
  // CAD-1027: the reviewed artifact text freeze will post, loaded for the
  // open confirmation (tagged by its approval id) — never the run title.
  const [reviewed, setReviewed] = useState<{
    approvalId: string;
    artifactId: string;
    text: string;
    digest: string;
  } | null>(null);
  const [confirmation, setConfirmation] = useState<{
    approvalId: string;
    epoch: number;
  } | null>(null);
  const [choice, setChoice] = useState<"now" | "schedule">("now");
  const [candidateId, setCandidateId] = useState("");
  const [toolkit, setToolkit] = useState<"instagram" | "facebook">("instagram");
  const [due, setDue] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  // CAD-1027: the imported, digest-verified media for one draft+channel.
  const [media, setMedia] = useState<{ key: string; digest: string; subject: string } | null>(null);
  const statusRef = useRef<HTMLHeadingElement>(null);
  const candidate =
    candidates.find((value) => value.run_id === candidateId) ?? null;
  const dueEpoch = parseDueEpoch(due);
  const grantReady = isGrantIdUsable(grantId);
  const imageMissing =
    toolkit === "instagram" && candidate !== null && !candidate.image_digest;
  // A draft with a reviewed image sends only with a media key that binds
  // it: the sender accepts (no key, no digest) or (key, digest), never half.
  const needsImport = candidate !== null && candidate.image_digest !== null;
  const mediaReady = media !== null && media.subject === `${candidateId}|${toolkit}`;
  const canReview =
    canWrite &&
    (!needsImport || mediaReady) &&
    grantReady &&
    candidate !== null &&
    !busy &&
    !imageMissing &&
    (choice === "now" || dueEpoch !== null);
  // Any edit after review voids the confirmation: a new review mints a
  // new approval for the new content.
  useEffect(() => {
    setConfirmation(null);
  }, [candidateId, toolkit, due, grantId, choice]);

  const refresh = useCallback(async () => {
    try {
      const reply = await client.list(installId, contextId);
      setIntents(reply.intents);
      setLoadError(null);
    } catch (error) {
      setLoadError(message(error));
    }
  }, [client, installId, contextId]);

  useEffect(() => {
    setIntents(null);
    setLoadError(null);
    void refresh();
  }, [refresh]);

  const importImage = async () => {
    if (!candidate || !canWrite || busy) return;
    setBusy(true);
    setActionError(null);
    setMedia(null);
    try {
      const receipt = (await imageReceipts(candidate.run_id)).find(
        (value) => value.slot === "image" && value.asset !== null,
      );
      if (!receipt?.asset) throw new Error("This draft has no reviewed image receipt to import.");
      const reply = await client.importMedia({
        request_id: `imp-${randomHex()}`,
        install_id: installId,
        ...(contextId ? { context_id: contextId } : {}),
        run_id: candidate.run_id,
        artifact_id: candidate.artifact_id,
        bundle_digest: candidate.bundle_digest,
        slot: "publication",
        toolkit,
        destination_id: PILOT_DESTINATION.account_id,
      });
      if (!importMatchesReviewed(reply, receipt.asset.digest))
        throw new Error(
          `The imported image (${reply.image_digest}) does not match the reviewed image (${receipt.asset.digest}). Nothing was scheduled.`,
        );
      setMedia({ key: reply.media_key, digest: reply.image_digest, subject: `${candidateId}|${toolkit}` });
    } catch (error) {
      setActionError(message(error));
    } finally {
      setBusy(false);
    }
  };

  const review = () => {
    if (!canReview) return;
    const epoch =
      choice === "now" ? Math.floor(Date.now() / 1000) : dueEpoch;
    if (epoch === null) return;
    setActionError(null);
    const approvalId = mintApprovalId();
    const artifactId = candidate?.artifact_id ?? "";
    setConfirmation({ approvalId, epoch });
    setReviewed(null);
    loadArtifact(artifactId).then(
      (artifact) => {
        if (artifact.id !== artifactId || typeof artifact.text !== "string") {
          setActionError("The reviewed caption does not match this draft. Nothing was scheduled.");
          return;
        }
        setReviewed({ approvalId, artifactId, text: artifact.text, digest: artifact.digest });
      },
      (error) => setActionError(message(error)),
    );
  };
  const reviewedReady =
    confirmation !== null &&
    candidate !== null &&
    reviewed !== null &&
    reviewed.approvalId === confirmation.approvalId &&
    reviewed.artifactId === candidate.artifact_id;

  const schedule = async () => {
    if (!candidate || !confirmation || !canReview || !reviewedReady) return;
    const { approvalId, epoch } = confirmation;
    const mode = choice;
    setBusy(true);
    setActionError(null);
    try {
      const reply = await client.schedule({
        request_id: approvalId,
        install_id: installId,
        ...(contextId ? { context_id: contextId } : {}),
        run_id: candidate.run_id,
        effect_id: candidate.effect_id,
        artifact_id: candidate.artifact_id,
        bundle_digest: candidate.bundle_digest,
        slot: "publication",
        destination_id: PILOT_DESTINATION.account_id,
        toolkit,
        ...(needsImport && media ? { media_key: media.key } : {}),
        grant_id: grantId,
        approval_id: approvalId,
        due_epoch: epoch,
        timezone: PUBLISH_TIMEZONE,
      });
      setConfirmation(null);
      setNotice(
        mode === "now"
          ? `Posted for dispatch as ${reply.intent.intent_id}.`
          : `Scheduled as ${reply.intent.intent_id} for ${dueLabel(epoch, PUBLISH_TIMEZONE)}.`,
      );
      statusRef.current?.focus();
      await refresh();
    } catch (error) {
      setActionError(refusalFromError(message(error)));
    } finally {
      setBusy(false);
    }
  };

  // CAD-1027: cancel in the intent's own frozen scope. An unscoped list
  // shows every context's intents; the panel's context would refuse them.
  const cancel = async (intent: PublishIntent) => {
    const intentId = intent.intent_id;
    setBusy(true);
    setActionError(null);
    try {
      await client.cancel(intentId, intent.install_id, intent.context_id);
      setConfirmCancel(null);
      setNotice(`Cancelled ${intentId} before dispatch. Nothing was sent.`);
      statusRef.current?.focus();
      await refresh();
    } catch (error) {
      setActionError(message(error));
    } finally {
      setBusy(false);
    }
  };

  const card = (intent: PublishIntent) => {
    const reading = reconcileReading(intent.state);
    const unconfirmed = showsUncertainReading(intent);
    // Writer/reviewer prefer the relay-joined envelope values, then the
    // approved-run candidates, never invented.
    const meta = candidates.find((value) => value.run_id === intent.run_id) ?? null;
    const writer = intent.writer ?? meta?.writer ?? "unknown writer";
    const reviewer = intent.reviewer ?? meta?.reviewer ?? "unknown reviewer";
    return (
    <article key={intent.intent_id} className="wa-panel wa-stack">
      <div className="wa-row">
        <h3>{PILOT_DESTINATION.handle}</h3>
        <span className="wa-status" data-tone={publishStateTone(intent.state)}>
          {publishStateText(intent.state)}
        </span>
      </div>
      <p className="wa-kicker">
        {intent.intent_id} · due {dueLabel(intent.due_epoch, intent.timezone)}
      </p>
      <p className="wa-kicker">
        {writer} → {reviewer}
      </p>
      <p className="wa-muted">
        Destination id echoed <code>{intent.destination_id}</code> (handle
        never relay-vouched)
      </p>
      <p className="wa-muted">
        Caption <code>{intent.caption_digest}</code>
        {intent.image_digest ? (
          <> · image <code>{intent.image_digest}</code></>
        ) : (
          " · text-only"
        )}{" "}
        · binding <code>{intent.frozen_digest}</code> · key{" "}
        <code>{intent.idempotency_key}</code>
      </p>
      {intent.state === "posted" && (
        <p className="wa-muted">
          Verified receipt bound to the approved revision
          {intent.permalink ? (
            <>
              {" · "}
              <code>{intent.permalink}</code>
            </>
          ) : null}
          . A bare success string never counts.
        </p>
      )}
      {intent.state === "refused" && intent.refusal && (
        <p className="wa-alert">{refusalCopy(intent.refusal)}</p>
      )}
      {/* Held surfaces state plus a human reason string, never a machine
        code (backend-confirmed): "dispatch authority differs from frozen
        approval" or "approved material changed since freeze". Raw, then
        the needs-human reading. */}
      {intent.state === "held" && intent.refusal?.message && (
        <p className="wa-alert">Dispatch report: {intent.refusal.message}</p>
      )}
      {intent.state === "processing" && !unconfirmed && (
        <p className="wa-muted">
          Provider call in flight — outcomes reconcile through the status
          query before any retry.
        </p>
      )}
      {unconfirmed && (
        <p className="wa-muted">
          Reads as uncertain: the status query found a lost response after
          accept while state stays processing. Reconcile the upstream
          ledger before any retry. Never duplicate a provider post.
        </p>
      )}
      {reading && <p className="wa-muted">{reading}</p>}
      {canCancel(intent.state) && (
        <div className="wa-row">
          {confirmCancel === intent.intent_id ? (
            <>
              <Button
                variant="danger"
                size="sm"
                loading={busy}
                disabled={!canWrite}
                onClick={() => void cancel(intent)}
              >
                Confirm cancel
              </Button>
              <Button
                size="sm"
                disabled={busy}
                onClick={() => setConfirmCancel(null)}
              >
                Keep
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              disabled={!canWrite || busy}
              onClick={() => setConfirmCancel(intent.intent_id)}
            >
              Cancel before dispatch
            </Button>
          )}
        </div>
      )}
    </article>
    );
  };

  return (
    <div className="wa-stack">
      <section className="wa-panel wa-stack" aria-labelledby="publish-new">
        <h2 id="publish-new">Publish decision</h2>
        <p className="wa-muted">
          Destination is operator-selected:{" "}
          <strong>
            {PILOT_DESTINATION.handle} · {PILOT_DESTINATION.account_id}
          </strong>
          . Never inferred from a source handle.
        </p>
        <p className="wa-kicker">
          Landed contract: send grants allow 1–10 uses · dispatch stays
          closed (send_disabled) until the operator enables it · scheduling
          freezes the intent, never sends · binding pins digest-form, caption
          re-resolves from the run at dispatch.
        </p>
        <div className="wa-field">
          <label htmlFor="publish-grant">AgenticOS send grant</label>
          <input
            id="publish-grant"
            className="wa-input"
            value={grantId}
            onChange={(event) => setGrantId(event.target.value)}
            placeholder="dpq_…"
            autoComplete="off"
            spellCheck={false}
            disabled={!canWrite || busy}
          />
        </div>
        {grantReady ? (
          <p className="wa-muted">
            Grant bound: <code>{grantId}</code>
          </p>
        ) : (
          <p className="wa-alert">
            {grantId
              ? "A send grant id is dpq_ followed by 8–64 letters, digits, _ or -."
              : "No publish grant is bound. Enter the AgenticOS send grant id before scheduling."}
          </p>
        )}
        {!candidates.length && (
          <p className="wa-empty">
            No approved drafts with reviewed artifacts yet. Approve a run
            first — digests derive from its reviewed material.
          </p>
        )}
        {!!candidates.length && (
          <>
            <div className="wa-field">
              <label htmlFor="publish-candidate">Approved draft</label>
              <Select
                value={candidateId}
                onChange={setCandidateId}
                options={candidates.map((value) => ({
                  value: value.run_id,
                  label: value.title,
                  hint: `r ${value.run_id}`,
                }))}
                placeholder="Choose an approved draft"
                aria-label="Approved draft"
                disabled={!canWrite || busy}
                full
              />
            </div>
            {candidate && (
              <div className="wa-panel wa-stack">
                <p>{candidate.caption}</p>
                <p className="wa-kicker">
                  {candidate.writer} → {candidate.reviewer}
                  {candidate.image_digest
                    ? " · reviewed image bound"
                    : " · text-only (Facebook only)"}
                </p>
              </div>
            )}
            <fieldset className="wa-field" disabled={!canWrite || busy}>
              <legend className="wa-kicker">Choose, then confirm</legend>
              <div className="wa-row" role="radiogroup" aria-label="Post now or schedule">
                {(["now", "schedule"] as const).map((value) => (
                  <label key={value} className="wa-row">
                    <input
                      type="radio"
                      name="publish-choice"
                      value={value}
                      checked={choice === value}
                      onChange={() => setChoice(value)}
                    />{" "}
                    {value === "now" ? "Post now" : "Schedule"}
                  </label>
                ))}
              </div>
              {choice === "schedule" && (
                <div className="wa-field">
                  <label htmlFor="publish-due">
                    Due time · {PUBLISH_TIMEZONE}
                  </label>
                  <input
                    id="publish-due"
                    className="wa-input"
                    type="datetime-local"
                    value={due}
                    onChange={(event) => setDue(event.target.value)}
                    required
                  />
                </div>
              )}
              <div className="wa-field">
                <label htmlFor="publish-toolkit">Destination channel</label>
                <Select
                  value={toolkit}
                  onChange={(value) =>
                    setToolkit(value === "facebook" ? "facebook" : "instagram")
                  }
                  options={[
                    { value: "instagram", label: "Instagram" },
                    { value: "facebook", label: "Facebook" },
                  ]}
                  aria-label="Destination channel"
                  disabled={!canWrite || busy}
                />
              </div>
            </fieldset>
            {confirmation && candidate ? (
              <section className="wa-panel wa-stack" aria-label="Confirm publish">
                <h3>Confirm this exact post</h3>
                <dl className="wa-stack">
                  <dt className="wa-kicker">Caption (reviewed text that will post)</dt>
                  <dd>
                    {reviewedReady && reviewed ? (
                      <>
                        <p className="wa-caption" style={{ whiteSpace: "pre-wrap" }}>
                          {reviewed.text}
                        </p>
                        <code>{reviewed.digest}</code>
                      </>
                    ) : (
                      "Loading the reviewed caption…"
                    )}
                  </dd>
                  <dt className="wa-kicker">Image digest</dt>
                  <dd>
                    {needsImport && media ? (
                      <>
                        <code>{media.digest}</code> · key <code>{media.key}</code>
                      </>
                    ) : (
                      "text-only"
                    )}
                  </dd>
                  <dt className="wa-kicker">Due</dt>
                  <dd>{dueLabel(confirmation.epoch, PUBLISH_TIMEZONE)}</dd>
                  <dt className="wa-kicker">Timezone</dt>
                  <dd>{PUBLISH_TIMEZONE}</dd>
                  <dt className="wa-kicker">Destination</dt>
                  <dd>
                    {toolkit} · {PILOT_DESTINATION.handle} ·{" "}
                    <code>{PILOT_DESTINATION.account_id}</code>
                  </dd>
                  <dt className="wa-kicker">Grant · approval</dt>
                  <dd>
                    <code>{grantId}</code> · <code>{confirmation.approvalId}</code>
                  </dd>
                </dl>
                <div className="wa-row">
                  <Button
                    variant="primary"
                    disabled={!canReview || !reviewedReady}
                    loading={busy}
                    onClick={() => void schedule()}
                  >
                    {choice === "now" ? "Confirm and post now" : "Confirm and schedule"}
                  </Button>
                  <Button disabled={busy} onClick={() => setConfirmation(null)}>
                    Back
                  </Button>
                </div>
              </section>
            ) : (
              <div className="wa-row">
                {needsImport && (
                  <Button
                    disabled={!canWrite || busy || mediaReady || imageMissing}
                    loading={busy}
                    onClick={() => void importImage()}
                  >
                    {mediaReady ? "Image imported" : "Import image"}
                  </Button>
                )}
                <Button variant="primary" disabled={!canReview} onClick={review}>
                  {choice === "now" ? "Review post now" : "Review schedule"}
                </Button>
              </div>
            )}
            {imageMissing && (
              <p className="wa-alert" data-tone="fail" role="alert">
                Instagram needs a reviewed provider-accessible image. Pick a
                draft with a bound image, or use Facebook text-only.
              </p>
            )}
          </>
        )}
        {actionError && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {actionError}
          </p>
        )}
        {notice && (
          <p className="wa-muted" role="status">
            {notice}
          </p>
        )}
      </section>

      <section aria-labelledby="publish-status">
        <h2 id="publish-status" ref={statusRef} tabIndex={-1}>
          Dispatch status
        </h2>
        {intents === null && !loadError && (
          <p className="wa-empty" role="status">
            Loading publish intents…
          </p>
        )}
        {loadError && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {loadError}
          </p>
        )}
        {intents !== null && !intents.length && (
          <p className="wa-empty">
            No publish intents yet. An approved decision appears here with its
            queued, processing and terminal states.
          </p>
        )}
        {intents !== null && intents.length > 0 && (
          <div className="wa-grid">
            {intents.map(card)}
          </div>
        )}
      </section>
    </div>
  );
}

export type { PublishState };
