import { PILOT_DESTINATION, dueLabel } from "./socialPublish";

/** The exact post an operator approves: reviewed caption text, image,
 *  due time, timezone, destination, grant and approval. */
export interface PublishConfirmationFacts {
  /** `null` while the reviewed caption loads. */
  caption: { text: string; digest: string } | null;
  /** `null` for a text-only post. `label`/`ref` name what binds the
   *  image: the media `key` at schedule, the reviewed `receipt` later. */
  image: { digest: string; label: string; ref: string } | null;
  dueEpoch: number;
  timezone: string;
  toolkit: string;
  destinationId: string;
  grantId: string;
  approvalId: string;
}

/** CAD-1027 confirmation facts, shared by schedule and CAD-1041 send-now
 *  so both decisions show the same post the same way. */
export default function PublishConfirmation({ facts }: { facts: PublishConfirmationFacts }) {
  return (
    <dl className="wa-stack">
      <dt className="wa-kicker">Caption (reviewed text that will post)</dt>
      <dd>
        {facts.caption ? (
          <>
            <p className="wa-caption" style={{ whiteSpace: "pre-wrap" }}>
              {facts.caption.text}
            </p>
            <code>{facts.caption.digest}</code>
          </>
        ) : (
          "Loading the reviewed caption…"
        )}
      </dd>
      <dt className="wa-kicker">Image digest</dt>
      <dd>
        {facts.image ? (
          <>
            <code>{facts.image.digest}</code> · {facts.image.label} <code>{facts.image.ref}</code>
          </>
        ) : (
          "text-only"
        )}
      </dd>
      <dt className="wa-kicker">Due</dt>
      <dd>{dueLabel(facts.dueEpoch, facts.timezone)}</dd>
      <dt className="wa-kicker">Timezone</dt>
      <dd>{facts.timezone}</dd>
      <dt className="wa-kicker">Destination</dt>
      <dd>
        {facts.toolkit} · {PILOT_DESTINATION.handle} ·{" "}
        <code>{facts.destinationId}</code>
      </dd>
      <dt className="wa-kicker">Grant · approval</dt>
      <dd>
        <code>{facts.grantId}</code> · <code>{facts.approvalId}</code>
      </dd>
    </dl>
  );
}
