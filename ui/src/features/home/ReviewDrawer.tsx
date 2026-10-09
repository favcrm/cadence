import { useEffect, useState, type ReactNode } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import Button from "../../ui/Button";
import { READ_ONLY_COPY } from "./AnswerForm";
import { useIdeaDecision } from "./IdeaCard";
import { useMergeDecision } from "./MergeForm";
import { usePlanDecision } from "./PlanCard";
import { ageWords, changeSize, firstSentence, type HomeNeed } from "./needs";

/**
 * The review drawer (CAD-1216): plans, ideas and merge decisions open
 * from the right edge over the whole board. One fixed order: title +
 * where/age, a one-sentence outcome, the content, "Checked" ticks, a
 * hidden Details link, and a pinned footer (primary, secondary, Not now,
 * "n of m"). Only facts the row carries are shown. The decisions run
 * through the same hooks the plan, idea and merge cards use, so the
 * operator-only routes, the revision binding and the pinned head are
 * unchanged.
 */
export interface ReviewProps {
  need: HomeNeed;
  readOnly: boolean;
  /** 1-based position among the reading items, and how many there are. */
  index: number;
  total: number;
  /** Done: the card is marked and the drawer advances (or closes after the last). */
  onDone: (key: string, text: string) => void;
  onClose: () => void;
  /** "Change something…": hand the thing to Master as a composer draft. */
  onAsk: (need: HomeNeed, lead: string) => void;
}

/** The project and the kind of decision, as the small line above the title. */
const kicker = (verb: string, need: HomeNeed) =>
  need.project && need.project !== "all" ? `${verb} · ${need.project}` : verb;

function useIssue(id: string) {
  const store = resources.issue(id);
  useEffect(() => {
    void store.revalidate();
  }, [store]);
  const state = useMaybeResource(store);
  return state?.data && state.data.id === id ? state.data : null;
}

function Details({ text }: { text: string }) {
  const [shown, setShown] = useState(false);
  return (
    <>
      <button type="button" className="rv-dlnk" aria-expanded={shown} onClick={() => setShown((s) => !s)}>
        Details
      </button>
      {shown && <pre className="rv-tech">{text}</pre>}
    </>
  );
}

function Ticks({ items }: { items: string[] }) {
  if (items.length === 0) return null;
  return (
    <>
      <h4 className="rv-h">Checked</h4>
      <ul className="rv-ticks">
        {items.map((t) => (
          <li key={t}>{t}</li>
        ))}
      </ul>
    </>
  );
}

/** The "send back" panel: a required reason, then the decision's own route. */
function SendBack({
  label,
  placeholder,
  busy,
  onSend,
}: {
  label: string;
  placeholder: string;
  busy: boolean;
  onSend: (reason: string) => void;
}) {
  const [reason, setReason] = useState("");
  return (
    <div className="rv-sendback">
      <textarea
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        onFocus={(e) => e.currentTarget.scrollIntoView?.({ block: "center" })}
        rows={3}
        placeholder={placeholder}
        aria-label={placeholder}
      />
      <Button size="sm" loading={busy} disabled={busy} onClick={() => onSend(reason)}>
        {label}
      </Button>
    </div>
  );
}

/** The pinned footer: primary, secondary, Not now, then "n of m". */
function Foot({
  primary,
  secondary,
  block,
  index,
  total,
  onClose,
}: {
  primary: ReactNode;
  secondary: ReactNode;
  block: string | null;
  index: number;
  total: number;
  onClose: () => void;
}) {
  return (
    <div className="rv-foot">
      {block && <p className="rv-readonly">{READ_ONLY_COPY}</p>}
      <div className="rv-actions">
        <span className="rv-primary">{primary}</span>
        {secondary}
        <Button variant="ghost" onClick={onClose}>
          Not now
        </Button>
        <span className="rv-count">
          {index} of {total}
        </span>
      </div>
    </div>
  );
}

function Shell({
  need,
  verb,
  title,
  subtitle,
  body,
  foot,
  onClose,
}: {
  need: HomeNeed;
  verb: string;
  title: string;
  subtitle: string;
  body: ReactNode;
  foot: ReactNode;
  onClose: () => void;
}) {
  return (
    <DrawerShell
      kind="review"
      scrim="board"
      label={verb}
      kicker={kicker(verb, need)}
      title={title}
      subtitle={subtitle}
      tabs={[{ id: "review", label: verb, panel: <div className="rv-body">{body}</div> }]}
      foot={foot}
      onClose={onClose}
    />
  );
}

const waiting = (need: HomeNeed) => `${ageWords(need.age)}${need.age < 60 ? "" : " ago"}`;

function PlanReview({ need, readOnly, index, total, onDone, onClose, onAsk }: ReviewProps) {
  const epic = need.action.type === "plan" ? need.action.epic : "";
  const block = useWriteBlock(readOnly);
  const [sendBack, setSendBack] = useState(false);
  const p = usePlanDecision(epic, block, (_, state) => onDone(need.key, state === "rejected" ? "Sent back" : "Plan approved"));
  const view = p.view;
  const open = view?.state === "proposed";
  const title = view?.title ?? need.shortTitle ?? "Plan";
  const lead = firstSentence(view?.goal);
  const body = !p.detail ? (
    <p className="rv-note">{p.state.status === "failed" ? "This plan could not be read." : "Reading the plan…"}</p>
  ) : !view ? (
    <p className="rv-note">This isn't a plan any more.</p>
  ) : (
    <>
      {lead && <p className="rv-lead">{lead}</p>}
      {!open && <p className="rv-note">This plan is already {view.state}.</p>}
      <h4 className="rv-h">The steps</h4>
      <ol className="rv-steps">
        {view.tickets.map((t, i) => (
          <li key={t.id}>
            <span className="rv-n">{i + 1}</span>
            <div>
              <b>{t.title}</b>
              <small>
                {[
                  typeof t.acceptance === "number" ? `${t.acceptance} ${t.acceptance === 1 ? "check" : "checks"} to pass` : null,
                  t.blocked_by && t.blocked_by.length > 0 ? "starts after an earlier step" : null,
                  t.status === "done" ? "done" : null,
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </small>
            </div>
          </li>
        ))}
        {view.tickets.length === 0 && <li className="rv-note">No steps are listed.</li>}
      </ol>
      {sendBack && open && (
        <SendBack
          label="Send back"
          placeholder="What should change?"
          busy={p.busy === "reject"}
          onSend={(r) => p.decide("reject", r)}
        />
      )}
      {p.error && (
        <p className="rv-error" role="alert">
          {p.error}
        </p>
      )}
      <Details text={`${epic} · ${view.total} tickets · proposed by ${view.proposedBy ?? "unknown"}${need.project ? ` · project ${need.project}` : ""}`} />
    </>
  );
  const off = !!block || !open || !p.detail;
  return (
    <Shell
      need={need}
      verb="Approve plan"
      title={title}
      subtitle={`Proposed by ${view?.proposedBy === "master" ? "Master" : "your team"} · ${waiting(need)}`}
      body={body}
      onClose={onClose}
      foot={
        <Foot
          block={block}
          index={index}
          total={total}
          onClose={onClose}
          primary={
            <Button variant="primary" size="md" loading={p.busy === "approve"} disabled={off || p.busy !== null} onClick={() => p.decide("approve")}>
              Approve plan
            </Button>
          }
          secondary={
            <>
              <Button
                disabled={!!block}
                title={block ? READ_ONLY_COPY : undefined}
                onClick={() => {
                  onAsk(need, `I'd like to change something in the plan "${title}": `);
                  onClose();
                }}
              >
                Change something…
              </Button>
              <Button disabled={off || p.busy !== null} onClick={() => setSendBack((s) => !s)}>
                Send back…
              </Button>
            </>
          }
        />
      }
    />
  );
}

function IdeaReview({ need, readOnly, index, total, onDone, onClose }: ReviewProps) {
  const issue = need.action.type === "idea" ? need.action.issue : "";
  const block = useWriteBlock(readOnly);
  const [mode, setMode] = useState<"reject" | "park" | null>(null);
  const [parkUntil, setParkUntil] = useState("");
  const d = useIdeaDecision(issue, (_, action) =>
    onDone(need.key, action === "reject" ? "Sent back" : action === "park" ? "Parked" : "Idea approved"),
  );
  const detail = d.detail;
  const lead = firstSentence(detail?.body);
  const off = !!block || !detail;
  return (
    <Shell
      need={need}
      verb="Approve idea"
      title={detail?.title ?? "Idea"}
      subtitle={`Waiting ${ageWords(need.age)}`}
      onClose={onClose}
      body={
        !detail ? (
          <p className="rv-note">{d.state.status === "failed" ? "This idea could not be read." : "Reading the idea…"}</p>
        ) : (
          <>
            {lead && <p className="rv-lead">{lead}</p>}
            <h4 className="rv-h">If you approve</h4>
            <p className="rv-note">The work the idea proposes is created and the team can start on it.</p>
            {mode === "reject" && (
              <SendBack
                label="Send back"
                placeholder="What is wrong with this idea?"
                busy={d.busy === "reject"}
                onSend={(r) => d.decide("reject", r, "")}
              />
            )}
            {mode === "park" && (
              <div className="rv-sendback">
                <label className="rv-note" htmlFor="rv-park">
                  Look at it again after
                </label>
                <input id="rv-park" type="date" className="field" value={parkUntil} onChange={(e) => setParkUntil(e.target.value)} />
                <Button size="sm" loading={d.busy === "park"} disabled={d.busy !== null} onClick={() => d.decide("park", "", parkUntil)}>
                  Park it
                </Button>
              </div>
            )}
            {d.error && (
              <p className="rv-error" role="alert">
                {d.error}
              </p>
            )}
            <button type="button" className="rv-dlnk" disabled={off} onClick={() => setMode(mode === "park" ? null : "park")}>
              Not ready? Park it for later
            </button>
            <Details text={`${issue} · project ${need.project ?? "—"}`} />
          </>
        )
      }
      foot={
        <Foot
          block={block}
          index={index}
          total={total}
          onClose={onClose}
          primary={
            <Button variant="primary" loading={d.busy === "approve"} disabled={off || d.busy !== null} onClick={() => d.decide("approve", "", "")}>
              Approve idea
            </Button>
          }
          secondary={
            <Button disabled={off || d.busy !== null} onClick={() => setMode(mode === "reject" ? null : "reject")}>
              Send back…
            </Button>
          }
        />
      }
    />
  );
}

function MergeReview({ need, readOnly, index, total, onDone, onClose }: ReviewProps) {
  const action = need.action as HomeNeed["action"] & { type: "merge" };
  const block = useWriteBlock(readOnly);
  const [sendBack, setSendBack] = useState(false);
  const issue = useIssue(action.issue);
  const m = useMergeDecision(need as HomeNeed & { action: { type: "merge" } }, (t) =>
    onDone(need.key, t.startsWith("declined") ? "Sent back" : "Published"),
  );
  const size = changeSize(need.title);
  const ticks = [
    action.reviewer ? "Reviewed and passed by a second agent" : null,
    action.verdict ? action.verdict : null,
    m.head ? "Checked at the exact version you are about to publish" : null,
  ].filter((t): t is string => !!t);
  const lead = firstSentence(issue?.body);
  const off = !!block || !m.head;
  return (
    <Shell
      need={need}
      verb="Approve change"
      title={issue?.title ?? need.shortTitle ?? "Publish a change"}
      subtitle={`Ready ${waiting(need)}`}
      onClose={onClose}
      body={
        <>
          {lead && <p className="rv-lead">{lead}</p>}
          {size && (
            <>
              <h4 className="rv-h">What changes</h4>
              <p className="rv-note">{size}</p>
            </>
          )}
          <Ticks items={ticks} />
          {!m.head && <p className="rv-error">This has no checked version to approve. It needs a fresh review.</p>}
          {sendBack && (
            <SendBack
              label="Send back"
              placeholder="What should change before this goes out?"
              busy={m.busy === "decline"}
              onSend={m.decline}
            />
          )}
          {m.error && (
            <p className="rv-error" role="alert">
              {m.error}
            </p>
          )}
          <Details
            text={[
              action.pr ?? action.issue,
              m.head ? `head ${m.head.slice(0, 12)}` : null,
              action.reviewer ? `reviewer ${action.reviewer}` : null,
              action.issue,
            ]
              .filter(Boolean)
              .join(" · ")}
          />
        </>
      }
      foot={
        <Foot
          block={block}
          index={index}
          total={total}
          onClose={onClose}
          primary={
            <Button variant="primary" loading={m.busy === "merge"} disabled={off || m.busy !== null} onClick={m.merge}>
              {m.busy === "merge" ? "Publishing…" : "Publish"}
            </Button>
          }
          secondary={
            <Button disabled={!!block || m.busy !== null} onClick={() => { setSendBack((s) => !s); m.clearError(); }}>
              Send back…
            </Button>
          }
        />
      }
    />
  );
}

/** The drawer for one reading item; the caller keys it by the item so each starts fresh. */
export default function ReviewDrawer(props: ReviewProps) {
  const t = props.need.action.type;
  if (t === "plan") return <PlanReview {...props} />;
  if (t === "idea") return <IdeaReview {...props} />;
  if (t === "merge") return <MergeReview {...props} />;
  return null;
}
