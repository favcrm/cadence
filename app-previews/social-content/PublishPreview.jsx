import { Fragment, useState } from "react";
import {
  publishCost,
  publishDestination,
  publishFrozen,
  publishStates,
} from "./publishFixtures.mjs";
import { fixtureTimeZone, fixtureWeek } from "./fixtures.mjs";

// CAD-787 interactive preview: human approves the exact destination and
// content digests, then chooses Post now or Schedule. Every control below is
// fixture-only simulation — buttons say Simulate, receipts are illustrated,
// and nothing contacts a provider. Product UI (after owner design acceptance)
// reuses this layout bound to the AOS-94 exact-destination contract.
function Facts({ entries }) {
  return (
    <dl
      style={{
        display: "grid",
        gridTemplateColumns: "minmax(72px, 100px) minmax(0, 1fr)",
        gap: "7px 12px",
        margin: "12px 0",
        fontSize: "var(--text-label)",
      }}
    >
      {entries.map(([label, value]) => (
        <Fragment key={label}>
          <dt className="muted">{label}</dt>
          <dd style={{ margin: 0, overflowWrap: "anywhere" }}>{value}</dd>
        </Fragment>
      ))}
    </dl>
  );
}

const flowNote =
  "Fixture simulation: no provider call, no credential, nothing published.";

export default function PublishPreview({ readonly = false }) {
  const [approved, setApproved] = useState(false);
  const [choice, setChoice] = useState("now");
  const [scheduleAt, setScheduleAt] = useState(
    `${fixtureWeek}T${publishFrozen.scheduleDefault}`,
  );
  const [dispatch, setDispatch] = useState(null);
  const [held, setHeld] = useState(false);
  // No stored uncertain state: a lost response keeps state processing and
  // shows the uncertain reading (reconcile first). See the reading tile.
  const [lostResponse, setLostResponse] = useState(false);
  const [notice, setNotice] = useState("");

  const frozen = publishFrozen;
  const destination = publishDestination;

  function simulatePostNow() {
    setDispatch("processing");
    setLostResponse(false);
    setNotice("Simulated send accepted into processing. " + flowNote);
  }
  function simulateSchedule() {
    setDispatch("queued");
    setLostResponse(false);
    setNotice(
      `Simulated schedule queued for ${scheduleAt || "no time chosen"} · ${fixtureTimeZone}. ` +
        flowNote,
    );
  }
  function resolveAs(outcome) {
    if (outcome === "lost-response") {
      // Uncertain reads as processing: state stays processing, the reading
      // orders reconcile-before-retry with no second send.
      setDispatch("processing");
      setLostResponse(true);
      setNotice(
        "Simulated lost response after accept — reads as uncertain while state stays processing. Reconcile before any retry. Nothing duplicated. " +
          flowNote,
      );
      return;
    }
    setDispatch(outcome);
    setLostResponse(false);
    setNotice(
      outcome === "posted"
        ? "Simulated verified receipt pinned to r3. " + flowNote
        : "Simulated refusal — nothing was published. " + flowNote,
    );
  }
  function resetFlow(message) {
    setApproved(false);
    setDispatch(null);
    setHeld(false);
    setLostResponse(false);
    setNotice(message);
  }

  const canDecide = approved && !dispatch && !held && !readonly;
  const scheduleValid = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(scheduleAt);

  return (
    <section aria-label="Publish decision preview">
      <p className="notice" role="status">
        Interactive preview · Fixtures only · {flowNote} Product UI lands only
        after owner design acceptance.
      </p>

      <div className="editor-grid">
        <section className="editor-fields">
          <div className="card pad">
            <h3>Exact destination</h3>
            <div className="row">
              <span className="chip tone">{destination.channel}</span>
              <strong>{destination.handle}</strong>
              <span className="badge b-acc">Fixture connected</span>
            </div>
            <Facts
              entries={[
                ["account", `${destination.accountKind} · ${destination.accountId}`],
                ["capability", destination.capability],
                ["scope", destination.scope],
                ["binding", destination.bindingRevision],
                ["grant", destination.grant],
              ]}
            />
            <p className="muted">{destination.statusNote}</p>
            <p className="muted">
              Selected by the operator — never inferred from the source handle
              (@juicysuite_crm is the source receipt, not the destination).
            </p>
          </div>

          <div className="card pad">
            <h3>
              Frozen content · {frozen.postId} r{frozen.revision}
            </h3>
            <p>{frozen.caption}</p>
            <p className="muted">Media: {frozen.media.join(", ")}</p>
            <Facts
              entries={[
                ["caption digest", frozen.captionDigest],
                ["media digest", frozen.mediaDigest],
                ["binding digest", frozen.frozenDigest],
                ["idempotency key", frozen.idempotencyKey],
                ["writer", frozen.writer],
                ["reviewer", `${frozen.reviewer} · ${frozen.reviewedAt}`],
              ]}
            />
            <p className="muted">
              Any caption, media, time or destination edit voids this approval
              and returns here.
            </p>
          </div>

          <div className="card pad">
            <h3>Cost & authority</h3>
            <Facts
              entries={[
                ["generation", `${publishCost.generationQuote} · ${publishCost.generationState}`],
                ["this publish", publishCost.publishCharge],
              ]}
            />
            <p className="muted">{publishCost.authorityNote}</p>
            <p className="muted">
              Landed contract: send grants allow 1–10 uses · dispatch stays
              closed (send_disabled is a refusal code, not a state) · binding
              pins digest-form, caption re-resolves at dispatch.
            </p>
          </div>

          <div className="card pad">
            <h3>Approve, then Post now or Schedule</h3>
            <label className="row" style={{ alignItems: "flex-start" }}>
              <input
                type="checkbox"
                checked={approved}
                disabled={readonly || !!dispatch || held}
                onChange={(e) => setApproved(e.target.checked)}
                aria-label="Approve the exact destination and content digests"
              />
              <span>
                I approve {destination.handle} receiving r{frozen.revision}{" "}
                with the digests above.
              </span>
            </label>
            <div className="row destinations" role="radiogroup" aria-label="Post now or schedule">
              {[
                ["now", "Post now"],
                ["schedule", "Schedule"],
              ].map(([value, label]) => (
                <label key={value}>
                  <input
                    type="radio"
                    name="publish-choice"
                    value={value}
                    checked={choice === value}
                    disabled={readonly || !!dispatch || held}
                    onChange={() => setChoice(value)}
                  />{" "}
                  {label}
                </label>
              ))}
            </div>
            {choice === "schedule" && (
              <>
                <label htmlFor="publish-at">
                  Due time · {fixtureTimeZone}
                </label>
                <input
                  id="publish-at"
                  className="field"
                  type="datetime-local"
                  value={scheduleAt}
                  disabled={readonly || !!dispatch || held}
                  onChange={(e) => setScheduleAt(e.target.value)}
                />
              </>
            )}
            <div className="row">
              {choice === "now" ? (
                <button
                  className="btn btn-primary"
                  disabled={!canDecide}
                  title="Fixture simulation — unlocks after approving the exact destination and digests"
                  onClick={simulatePostNow}
                >
                  Simulate Post now
                </button>
              ) : (
                <button
                  className="btn btn-primary"
                  disabled={!canDecide || !scheduleValid}
                  title="Fixture simulation — unlocks after approving the exact destination and digests"
                  onClick={simulateSchedule}
                >
                  Simulate Schedule
                </button>
              )}
              <button
                className="btn btn-sm"
                disabled={readonly || held || !approved || !!dispatch}
                title="Fixture: illustrate the dispatch-time authority recheck failing"
                onClick={() => {
                  setHeld(true);
                  setNotice(
                    "Simulated authority change at dispatch — held for a new human decision. Never silently published, never auto-resumed. " +
                      flowNote,
                  );
                }}
              >
                Simulate authority change
              </button>
            </div>
            {!approved && (
              <p className="muted">
                Post now / Schedule unlock only after approving the exact
                destination and digests.
              </p>
            )}
          </div>

          {(dispatch || held) && (
            <div className="card pad">
              <h3>
                Dispatch ·{" "}
                {held ? "held" : dispatch === "queued" ? "queued" : dispatch}
              </h3>
              {held ? (
                <>
                  <p className="error" role="alert">
                    Held: grant, binding, app or context changed at dispatch.
                    Awaiting a new human decision — reconnecting the provider
                    alone never resumes the intent; re-approve after reconnect.
                  </p>
                  <div className="row">
                    <button
                      className="btn"
                      disabled={readonly}
                      onClick={() => {
                        setHeld(false);
                        setApproved(false);
                        setNotice(
                          "Simulated reconnect complete — re-approve the (possibly new) binding before sending. " +
                            flowNote,
                        );
                      }}
                    >
                      Simulate reconnect
                    </button>
                    <button
                      className="btn btn-sm"
                      onClick={() =>
                        resetFlow("Simulated hold cleared back to review. " + flowNote)
                      }
                    >
                      Back to review
                    </button>
                  </div>
                </>
              ) : (
                <>
                  <p className="notice" role="status">
                    {dispatch === "queued" &&
                      `Simulated queued for ${scheduleAt} · ${fixtureTimeZone} · key ${frozen.idempotencyKey}.`}
                    {dispatch === "processing" && !lostResponse &&
                      "Simulated provider call in flight — reconcile before any retry."}
                    {dispatch === "processing" && lostResponse &&
                      "Simulated lost response after accept — reads as uncertain while state stays processing. Reconcile the upstream ledger before any retry; a recheck mismatch would store held."}
                    {dispatch === "posted" &&
                      "Simulated posted · permalink https://www.instagram.com/p/fixture000/ · receipt bound to r3."}
                    {dispatch === "refused" &&
                      "Simulated refusal — nothing was published."}
                    {dispatch === "cancelled" &&
                      "Simulated cancellation before dispatch — nothing was sent."}
                  </p>
                  <div className="row">
                    {dispatch === "queued" && (
                      <button
                        className="btn"
                        onClick={() => resolveAs("cancelled")}
                      >
                        Simulate cancel
                      </button>
                    )}
                    {dispatch === "processing" && !lostResponse && (
                      <>
                        <button
                          className="btn btn-primary"
                          onClick={() => resolveAs("posted")}
                        >
                          Simulate posted
                        </button>
                        <button className="btn" onClick={() => resolveAs("refused")}>
                          Simulate refused
                        </button>
                        <button
                          className="btn"
                          onClick={() => resolveAs("lost-response")}
                        >
                          Simulate lost response
                        </button>
                      </>
                    )}
                    {(dispatch === "posted" ||
                      dispatch === "refused" ||
                      (dispatch === "processing" && lostResponse) ||
                      dispatch === "cancelled") && (
                      <button
                        className="btn btn-sm"
                        onClick={() =>
                          resetFlow("Simulated flow reset to review. " + flowNote)
                        }
                      >
                        Back to review
                      </button>
                    )}
                  </div>
                </>
              )}
            </div>
          )}
          {notice && (
            <p className="notice" role="status">
              {notice}
            </p>
          )}
        </section>

        <section className="social-preview">
          <p className="slabel">Preview · exact post as the destination sees it</p>
          <div className="card">
            <header className="preview-head">
              <strong>{destination.handle}</strong>
              <span className="muted">{destination.channel} · fixture</span>
            </header>
            <div className="fixture-media">
              <strong>{frozen.media[0]}</strong>
              <span>Retained bytes · {frozen.mediaDigest}</span>
            </div>
            <div className="preview-caption">
              <p>{frozen.caption}</p>
              <p className="num muted">
                {choice === "schedule"
                  ? `${scheduleAt || "Unplanned"} · ${fixtureTimeZone}`
                  : "Post now"}{" "}
                · {destination.handle}
              </p>
            </div>
          </div>
          <p className="muted">
            External receipts: none. The posted permalink above is illustrated
            only after Simulate posted.
          </p>
        </section>
      </div>

      <h3 style={{ marginTop: 20 }}>All dispatch states</h3>
      <p className="muted">
        Every stored state the product UI must render, plus the uncertain
        reading (processing-or-held, never stored) — same tokens, same card
        components.
      </p>
      <div className="source-grid">
        {publishStates.map((state) => (
          <article className="card pad" key={state.id}>
            <div className="row">
              <h3 style={{ marginRight: "auto" }}>{state.title}</h3>
              <span className={`badge ${state.tone}`.trim()}>{state.badge ?? state.id}</span>
            </div>
            <p className="muted">{state.copy}</p>
            <div className="row">
              {state.actions.map((action) => (
                <button
                  key={action}
                  className="btn btn-sm"
                  disabled
                  title="Fixture state illustration — no action is connected"
                >
                  {action}
                </button>
              ))}
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
