import { useCallback, useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import PublishConfirmation from "./PublishConfirmation";
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
import { useTranslate } from "../../lib/locale";
import { displayUiNotice, type UiNotice } from "../../lib/uiNotice";

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
  const t = useTranslate();
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
  // CAD-1041: send-now takes two presses. The first loads the frozen
  // post (reviewed caption and image receipt, checked against the frozen
  // digests) into the CAD-1027 confirmation; only then can the second
  // press send. One click can never send.
  const [sendReview, setSendReview] = useState<{
    intentId: string;
    caption: { text: string; digest: string } | null;
    image: { digest: string; label: string; ref: string } | null;
    ready: boolean;
    error: string | null;
  } | null>(null);
  const [notice, setNotice] = useState<UiNotice | null>(null);
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
          ? { key: "Posted for dispatch as {intent}.", slots: { intent: reply.intent.intent_id } }
          : { key: "Scheduled as {intent} for {due}.", slots: { intent: reply.intent.intent_id, due: dueLabel(epoch, PUBLISH_TIMEZONE) } },
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
      setNotice({ key: "Cancelled {intent} before dispatch. Nothing was sent.", slots: { intent: intentId } });
      statusRef.current?.focus();
      await refresh();
    } catch (error) {
      setActionError(message(error));
    } finally {
      setBusy(false);
    }
  };

  // CAD-1041: send-now in the intent's own frozen scope, as cancel.
  // The frozen post as the operator will send it, or why it cannot be shown.
  const frozenPost = async (intent: PublishIntent) => {
    if (!intent.artifact_id)
      throw new Error("This intent names no reviewed caption. Nothing was sent.");
    const artifact = await loadArtifact(intent.artifact_id);
    if (
      artifact.id !== intent.artifact_id ||
      typeof artifact.text !== "string" ||
      artifact.digest !== `sha256:${intent.caption_digest}`
    )
      throw new Error("The reviewed caption no longer matches this intent. Nothing was sent.");
    let image: { digest: string; label: string; ref: string } | null = null;
    if (intent.image_digest) {
      const receipt = (await imageReceipts(intent.run_id)).find(
        (value) => value.slot === "image" && value.asset?.digest === `sha256:${intent.image_digest}`,
      );
      if (!receipt)
        throw new Error("No reviewed image receipt matches this intent's image. Nothing was sent.");
      image = { digest: intent.image_digest, label: "receipt", ref: receipt.id };
    }
    return { caption: { text: artifact.text, digest: artifact.digest }, image };
  };

  const armSend = (intent: PublishIntent) => {
    const intentId = intent.intent_id;
    setActionError(null);
    setSendReview({ intentId, caption: null, image: null, ready: false, error: null });
    const settle = (patch: Partial<NonNullable<typeof sendReview>>) =>
      setSendReview((current) =>
        current && current.intentId === intentId ? { ...current, ...patch } : current,
      );
    frozenPost(intent).then(
      ({ caption, image }) => settle({ caption, image, ready: true }),
      (error) => settle({ error: message(error) }),
    );
  };

  const sendNow = async (intent: PublishIntent) => {
    const intentId = intent.intent_id;
    setBusy(true);
    setActionError(null);
    try {
      const reply = await client.sendNow(intentId, intent.install_id, intent.context_id);
      setSendReview(null);
      setNotice(
        reply.intent.state === "posted"
          ? { key: "Sent {intent} — posted.", slots: { intent: intentId } }
          : { key: "Send-now for {intent} left it {state}.", slots: { intent: intentId, state: reply.intent.state } }
      );
      statusRef.current?.focus();
      await refresh();
    } catch (error) {
      setActionError(message(error));
    } finally {
      setBusy(false);
    }
  };

  const card = (intent: PublishIntent) => {
    const reading = reconcileReading(intent.state, t);
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
          {publishStateText(intent.state, t)}
        </span>
      </div>
      <p className="wa-kicker">
        {intent.intent_id} · {t("due")} {dueLabel(intent.due_epoch, intent.timezone)}
      </p>
      <p className="wa-kicker">
        {writer} → {reviewer}
      </p>
      <p className="wa-muted">
        {t("Destination id echoed")} <code>{intent.destination_id}</code> {t("(handle never relay-vouched)")}
      </p>
      <p className="wa-muted">
        {t("Caption")} <code>{intent.caption_digest}</code>
        {intent.image_digest ? (
          <> · {t("image")} <code>{intent.image_digest}</code></>
        ) : (
          t(" · text-only")
        )}{" "}
        · {t("binding")} <code>{intent.frozen_digest}</code> · {t("key")}{" "}
        <code>{intent.idempotency_key}</code>
      </p>
      {intent.state === "posted" && (
        <p className="wa-muted">
          {t("Verified receipt bound to the approved revision")}
          {intent.permalink ? (
            <>
              {" · "}
              <code>{intent.permalink}</code>
            </>
          ) : null}
          {t(". A bare success string never counts.")}
        </p>
      )}
      {intent.state === "refused" && intent.refusal && (
        <p className="wa-alert">{refusalCopy(intent.refusal, t)}</p>
      )}
      {/* Held surfaces state plus a human reason string, never a machine
        code (backend-confirmed): "dispatch authority differs from frozen
        approval" or "approved material changed since freeze". Raw, then
        the needs-human reading. */}
      {intent.state === "held" && intent.refusal?.message && (
        <p className="wa-alert">{t("Dispatch report:")} {intent.refusal.message}</p>
      )}
      {intent.state === "processing" && !unconfirmed && (
        <p className="wa-muted">
          {t("Provider call in flight — outcomes reconcile through the status query before any retry.")}
        </p>
      )}
      {unconfirmed && (
        <p className="wa-muted">
          {t("Reads as uncertain: the status query found a lost response after accept while state stays processing. Reconcile the upstream ledger before any retry. Never duplicate a provider post.")}
        </p>
      )}
      {reading && <p className="wa-muted">{reading}</p>}
      {intent.state === "queued" &&
        (sendReview?.intentId === intent.intent_id ? (
          <section className="wa-panel wa-stack" aria-label={t("Confirm send now")}>
            <h4>{t("Send this exact post now")}</h4>
            {sendReview.error ? (
              <p className="wa-alert" data-tone="fail" role="alert">
                {sendReview.error}
              </p>
            ) : (
              <PublishConfirmation
                facts={{
                  caption: sendReview.caption,
                  image: sendReview.image,
                  dueEpoch: intent.due_epoch,
                  timezone: intent.timezone,
                  toolkit: intent.channel,
                  destinationId: intent.destination_id,
                  grantId: intent.grant_id,
                  approvalId: intent.approval_id,
                }}
              />
            )}
            <div className="wa-row">
              <Button
                variant="danger"
                size="sm"
                loading={busy}
                disabled={!canWrite || !sendReview.ready || sendReview.error !== null}
                onClick={() => void sendNow(intent)}
              >
                {t("Confirm send now")}
              </Button>
              <Button size="sm" disabled={busy} onClick={() => setSendReview(null)}>
                {t("Keep queued")}
              </Button>
            </div>
          </section>
        ) : (
          <div className="wa-row">
            <Button size="sm" disabled={!canWrite || busy} onClick={() => armSend(intent)}>
              {t("Send now")}
            </Button>
          </div>
        ))}
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
                {t("Confirm cancel")}
              </Button>
              <Button
                size="sm"
                disabled={busy}
                onClick={() => setConfirmCancel(null)}
              >
                {t("Keep")}
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              disabled={!canWrite || busy}
              onClick={() => setConfirmCancel(intent.intent_id)}
            >
              {t("Cancel before dispatch")}
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
        <h2 id="publish-new">{t("Publish decision")}</h2>
        <p className="wa-muted">
          {t("Destination is operator-selected:")}{" "}
          <strong>
            {PILOT_DESTINATION.handle} · {PILOT_DESTINATION.account_id}
          </strong>
          {t(". Never inferred from a source handle.")}
        </p>
        <p className="wa-kicker">
          {t("Landed contract: send grants allow 1–10 uses · dispatch stays closed (send_disabled) until the operator enables it · scheduling freezes the intent, never sends · binding pins digest-form, caption re-resolves from the run at dispatch.")}
        </p>
        <div className="wa-field">
          <label htmlFor="publish-grant">{t("AgenticOS send grant")}</label>
          <input
            id="publish-grant"
            className="wa-input"
            value={grantId}
            onChange={(event) => setGrantId(event.target.value)}
            placeholder={t("dpq_…")}
            autoComplete="off"
            spellCheck={false}
            disabled={!canWrite || busy}
          />
        </div>
        {grantReady ? (
          <p className="wa-muted">
            {t("Grant bound:")} <code>{grantId}</code>
          </p>
        ) : (
          <p className="wa-alert">
            {grantId
              ? t("A send grant id is dpq_ followed by 8–64 letters, digits, _ or -.")
              : t("No publish grant is bound. Enter the AgenticOS send grant id before scheduling.")}
          </p>
        )}
        {!candidates.length && (
          <p className="wa-empty">
            {t("No approved drafts with reviewed artifacts yet. Approve a run first — digests derive from its reviewed material.")}
          </p>
        )}
        {!!candidates.length && (
          <>
            <div className="wa-field">
              <label htmlFor="publish-candidate">{t("Approved draft")}</label>
              <Select
                value={candidateId}
                onChange={setCandidateId}
                options={candidates.map((value) => ({
                  value: value.run_id,
                  label: value.title,
                  hint: `r ${value.run_id}`,
                }))}
                placeholder={t("Choose an approved draft")}
                aria-label={t("Approved draft")}
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
                    ? t(" · reviewed image bound")
                    : t(" · text-only (Facebook only)")}
                </p>
              </div>
            )}
            <fieldset className="wa-field" disabled={!canWrite || busy}>
              <legend className="wa-kicker">{t("Choose, then confirm")}</legend>
              <div className="wa-row" role="radiogroup" aria-label={t("Post now or schedule")}>
                {(["now", "schedule"] as const).map((value) => (
                  <label key={value} className="wa-row">
                    <input
                      type="radio"
                      name="publish-choice"
                      value={value}
                      checked={choice === value}
                      onChange={() => setChoice(value)}
                    />{" "}
                    {value === "now" ? t("Post now") : t("Schedule")}
                  </label>
                ))}
              </div>
              {choice === "schedule" && (
                <div className="wa-field">
                  <label htmlFor="publish-due">
                    {t("Due time")} · {PUBLISH_TIMEZONE}
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
                <label htmlFor="publish-toolkit">{t("Destination channel")}</label>
                <Select
                  value={toolkit}
                  onChange={(value) =>
                    setToolkit(value === "facebook" ? "facebook" : "instagram")
                  }
                  options={[
                    { value: "instagram", label: "Instagram" },
                    { value: "facebook", label: "Facebook" },
                  ]}
                  aria-label={t("Destination channel")}
                  disabled={!canWrite || busy}
                />
              </div>
            </fieldset>
            {confirmation && candidate ? (
              <section className="wa-panel wa-stack" aria-label={t("Confirm publish")}>
                <h3>{t("Confirm this exact post")}</h3>
                <PublishConfirmation
                  facts={{
                    caption:
                      reviewedReady && reviewed
                        ? { text: reviewed.text, digest: reviewed.digest }
                        : null,
                    image:
                      needsImport && media
                        ? { digest: media.digest, label: "key", ref: media.key }
                        : null,
                    dueEpoch: confirmation.epoch,
                    timezone: PUBLISH_TIMEZONE,
                    toolkit,
                    destinationId: PILOT_DESTINATION.account_id,
                    grantId,
                    approvalId: confirmation.approvalId,
                  }}
                />
                <div className="wa-row">
                  <Button
                    variant="primary"
                    disabled={!canReview || !reviewedReady}
                    loading={busy}
                    onClick={() => void schedule()}
                  >
                    {choice === "now" ? t("Confirm and post now") : t("Confirm and schedule")}
                  </Button>
                  <Button disabled={busy} onClick={() => setConfirmation(null)}>
                    {t("Back")}
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
                    {mediaReady ? t("Image imported") : t("Import image")}
                  </Button>
                )}
                <Button variant="primary" disabled={!canReview} onClick={review}>
                  {choice === "now" ? t("Review post now") : t("Review schedule")}
                </Button>
              </div>
            )}
            {imageMissing && (
              <p className="wa-alert" data-tone="fail" role="alert">
                {t("Instagram needs a reviewed provider-accessible image. Pick a draft with a bound image, or use Facebook text-only.")}
              </p>
            )}
          </>
        )}
        {actionError && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {t(actionError)}
          </p>
        )}
        {notice && (
          <p className="wa-muted" role="status">
            {displayUiNotice(notice, t)}
          </p>
        )}
      </section>

      <section aria-labelledby="publish-status">
        <h2 id="publish-status" ref={statusRef} tabIndex={-1}>
          {t("Dispatch status")}
        </h2>
        {intents === null && !loadError && (
          <p className="wa-empty" role="status">
            {t("Loading publish intents…")}
          </p>
        )}
        {loadError && (
          <p className="wa-alert" data-tone="fail" role="alert">
            {t(loadError)}
          </p>
        )}
        {intents !== null && !intents.length && (
          <p className="wa-empty">
            {t("No publish intents yet. An approved decision appears here with its queued, processing and terminal states.")}
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
