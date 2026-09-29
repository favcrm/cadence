import { useCallback, useEffect, useRef, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import {
  PILOT_DESTINATION,
  PUBLISH_TIMEZONE,
  canCancel,
  dueLabel,
  isApprovalIdUsable,
  parseDueEpoch,
  publishStateText,
  publishStateTone,
  reconcileReading,
  refusalCopy,
  showsUncertainReading,
  socialPublish,
  type PublishIntent,
  type PublishState,
} from "./socialPublish";

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
 *  CAD-771/AOS-94 exact-destination states. Human approves the frozen
 *  digest set, then chooses Post now (`due_epoch` at now) or Schedule (a
 *  future `due_epoch` with timezone). Digests always derive server-side
 *  from the approved run artifact — the UI never invents one. */
export default function PublishPanel({
  installId,
  contextId,
  candidates,
  grantId,
  approvalId,
  canWrite,
  client = socialPublish,
}: {
  installId: string;
  contextId: string | null;
  candidates: PublishCandidate[];
  grantId: string;
  approvalId: string;
  canWrite: boolean;
  client?: typeof socialPublish;
}) {
  const [intents, setIntents] = useState<PublishIntent[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [approved, setApproved] = useState(false);
  const [choice, setChoice] = useState<"now" | "schedule">("now");
  const [candidateId, setCandidateId] = useState("");
  const [toolkit, setToolkit] = useState<"instagram" | "facebook">("instagram");
  const [due, setDue] = useState("");
  const [busy, setBusy] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const statusRef = useRef<HTMLHeadingElement>(null);
  const candidate =
    candidates.find((value) => value.run_id === candidateId) ?? null;
  const dueEpoch = parseDueEpoch(due);
  const approvalOversize = approvalId.length > 120;
  const grantReady =
    grantId.trim().length > 0 && isApprovalIdUsable(approvalId);
  const canDecide =
    canWrite && grantReady && candidate !== null && !busy && approved;

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

  const schedule = async (mode: "now" | "schedule") => {
    if (!candidate || !canDecide) return;
    const epoch =
      mode === "now" ? Math.floor(Date.now() / 1000) : dueEpoch;
    if (epoch === null) return;
    if (toolkit === "instagram" && !candidate.image_digest) return;
    setBusy(true);
    setActionError(null);
    try {
      const reply = await client.schedule({
        request_id: `${installId}.${candidate.run_id}.${epoch}`,
        install_id: installId,
        ...(contextId ? { context_id: contextId } : {}),
        run_id: candidate.run_id,
        effect_id: candidate.effect_id,
        artifact_id: candidate.artifact_id,
        bundle_digest: candidate.bundle_digest,
        slot: "publication",
        destination_id: PILOT_DESTINATION.account_id,
        toolkit,
        grant_id: grantId,
        approval_id: approvalId,
        due_epoch: epoch,
        timezone: PUBLISH_TIMEZONE,
      });
      setApproved(false);
      setNotice(
        mode === "now"
          ? `Posted for dispatch as ${reply.intent.intent_id}.`
          : `Scheduled as ${reply.intent.intent_id} for ${dueLabel(epoch, PUBLISH_TIMEZONE)}.`,
      );
      statusRef.current?.focus();
      await refresh();
    } catch (error) {
      setActionError(message(error));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async (intentId: string) => {
    setBusy(true);
    setActionError(null);
    try {
      await client.cancel(intentId);
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
                onClick={() => void cancel(intent.intent_id)}
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
        {approvalOversize && (
          <p className="wa-alert" data-tone="fail" role="alert">
            The approval identity exceeds 120 characters — the contract
            refuses it (grant_approval). Bind a shorter approval identity.
          </p>
        )}
        {!grantReady && (
          <p className="wa-alert">
            No publish grant is bound. Connect the destination and bind a send
            grant before scheduling — approval stays disabled until then.
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
              <legend className="wa-kicker">Approve, then choose</legend>
              <label className="wa-row">
                <input
                  type="checkbox"
                  checked={approved}
                  onChange={(event) => setApproved(event.target.checked)}
                  aria-label="Approve the exact destination and content digests"
                />
                <span>
                  I approve {PILOT_DESTINATION.handle} receiving this exact
                  frozen revision. Only the operator&apos;s press releases it.
                </span>
              </label>
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
            <div className="wa-row">
              {choice === "now" ? (
                <Button
                  variant="primary"
                  disabled={!canDecide}
                  loading={busy}
                  onClick={() => void schedule("now")}
                >
                  Post now
                </Button>
              ) : (
                <Button
                  variant="primary"
                  disabled={!canDecide || dueEpoch === null}
                  loading={busy}
                  onClick={() => void schedule("schedule")}
                >
                  Schedule
                </Button>
              )}
            </div>
            {toolkit === "instagram" && candidate && !candidate.image_digest && (
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
