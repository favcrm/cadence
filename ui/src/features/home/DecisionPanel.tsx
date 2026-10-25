import { useEffect, useState, type ReactNode } from "react";
import { useWriteBlock } from "../auth/WriteGate";
import { resources } from "../../lib/resources";
import { useMaybeResource } from "../../lib/useResource";
import Button from "../../ui/Button";
import { READ_ONLY_COPY } from "./AnswerForm";
import { useTranslate } from "../../lib/locale";
import { useApproveHead } from "./ApproveHead";
import { useIdeaDecision } from "./IdeaCard";
import { useMergeDecision } from "./MergeForm";
import { usePlanDecision } from "./PlanCard";
import { ageWords, changeSize, firstSentence, type HomeNeed } from "./needs";

/**
 * The Home decision panel (CAD-1216, hosted by the shared work-item detail
 * since CAD-1811): plans, ideas and merge decisions render inside the one
 * detail drawer, above the issue's recorded facts. One fixed order: title +
 * where/age, a one-sentence outcome, the content, "Checked" ticks, a hidden
 * Details link, and the decision footer (primary, secondary, Not now,
 * "n of m"). Only facts the row carries are shown. The decisions run
 * through the same hooks the plan, idea and merge cards use, so the
 * operator-only routes, the revision binding and the pinned head are
 * unchanged; the shared detail adds no approval path.
 */
export interface DecisionPanelProps {
  need: HomeNeed;
  readOnly: boolean;
  /** 1-based position among the reading items, and how many there are. */
  index: number;
  total: number;
  /** Done: the card is marked and the detail advances (or closes after the last). */
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
  const t = useTranslate();
  const [shown, setShown] = useState(false);
  return (
    <>
      <button type="button" className="rv-dlnk" aria-expanded={shown} onClick={() => setShown((s) => !s)}>
        {t("Details")}
      </button>
      {shown && <pre className="rv-tech">{text}</pre>}
    </>
  );
}

function Ticks({ items }: { items: string[] }) {
  const t = useTranslate();
  if (items.length === 0) return null;
  return (
    <>
      <h4 className="rv-h">{t("Checked")}</h4>
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
  const t = useTranslate();
  const [reason, setReason] = useState("");
  return (
    <div className="rv-sendback">
      <textarea
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        onFocus={(e) => e.currentTarget.scrollIntoView?.({ block: "center" })}
        rows={3}
        placeholder={t(placeholder)}
        aria-label={t(placeholder)}
      />
      <Button size="sm" loading={busy} disabled={busy} onClick={() => onSend(reason)}>
        {t(label)}
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
  const t = useTranslate();
  return (
    <div className="rv-foot">
      {block && <p className="rv-readonly">{t(READ_ONLY_COPY)}</p>}
      <div className="rv-actions">
        <span className="rv-primary">{primary}</span>
        {secondary}
        <Button variant="ghost" onClick={onClose}>
          {t("Not now")}
        </Button>
        <span className="rv-count">
          {index} {t("of")} {total}
        </span>
      </div>
    </div>
  );
}

/** The decision, rendered inside the shared detail's Overview panel. */
function Shell({
  need,
  verb,
  title,
  subtitle,
  body,
  foot,
}: {
  need: HomeNeed;
  verb: string;
  title: string;
  subtitle: string;
  body: ReactNode;
  foot: ReactNode;
}) {
  const t = useTranslate();
  return (
    <section className="rv-embedded" aria-label={t(verb)} data-review-verb={verb}>
      <div className="kicker">{kicker(t(verb), need)}</div>
      <h4 className="rv-embedded-title">{title}</h4>
      <p className="rv-embedded-sub">{subtitle}</p>
      <div className="rv-body">{body}</div>
      {foot}
    </section>
  );
}

const waiting = (need: HomeNeed, t: (s: string) => string) => `${t(ageWords(need.age))}${need.age < 60 ? "" : ` ${t("ago")}`}`;

function PlanReview({ need, readOnly, index, total, onDone, onClose, onAsk }: DecisionPanelProps) {
  const epic = need.action.type === "plan" ? need.action.epic : "";
  const block = useWriteBlock(readOnly);
  const t = useTranslate();
  const [sendBack, setSendBack] = useState(false);
  const p = usePlanDecision(epic, block, (_, state) => onDone(need.key, state === "rejected" ? "Sent back" : "Plan approved"));
  const view = p.view;
  const open = view?.state === "proposed";
  const title = view?.title ?? need.shortTitle ?? "Plan";
  const lead = firstSentence(view?.goal);
  const body = !p.detail ? (
    <p className="rv-note">{p.state.status === "failed" ? t("This plan could not be read.") : t("Reading the plan…")}</p>
  ) : !view ? (
    <p className="rv-note">{t("This isn't a plan any more.")}</p>
  ) : (
    <>
      {lead && <p className="rv-lead">{lead}</p>}
      {!open && <p className="rv-note">{t("This plan is already")} {view.state}.</p>}
      <h4 className="rv-h">{t("The steps")}</h4>
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
        {view.tickets.length === 0 && <li className="rv-note">{t("No steps are listed.")}</li>}
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
      subtitle={`${t("Proposed by")} ${view?.proposedBy === "master" ? t("Master") : t("your team")} · ${waiting(need, t)}`}
      body={body}
      foot={
        <Foot
          block={block}
          index={index}
          total={total}
          onClose={onClose}
          primary={
            <Button variant="primary" size="md" loading={p.busy === "approve"} disabled={off || p.busy !== null} onClick={() => p.decide("approve")}>
              {t("Approve plan")}
            </Button>
          }
          secondary={
            <>
              <Button
                disabled={!!block}
                title={block ? t(READ_ONLY_COPY) : undefined}
                onClick={() => {
                  onAsk(need, `I'd like to change something in the plan "${title}": `);
                  onClose();
                }}
              >
                {t("Change something…")}
              </Button>
              <Button disabled={off || p.busy !== null} onClick={() => setSendBack((s) => !s)}>
                {t("Send back…")}
              </Button>
            </>
          }
        />
      }
    />
  );
}

function IdeaReview({ need, readOnly, index, total, onDone, onClose }: DecisionPanelProps) {
  const issue = need.action.type === "idea" ? need.action.issue : "";
  const block = useWriteBlock(readOnly);
  const t = useTranslate();
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
      subtitle={`${t("Waiting")} ${t(ageWords(need.age))}`}
      body={
        !detail ? (
          <p className="rv-note">{d.state.status === "failed" ? t("This idea could not be read.") : t("Reading the idea…")}</p>
        ) : (
          <>
            {lead && <p className="rv-lead">{lead}</p>}
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
                  {t("Look at it again after")}
                </label>
                <input id="rv-park" type="date" className="field" value={parkUntil} onChange={(e) => setParkUntil(e.target.value)} />
                <Button size="sm" loading={d.busy === "park"} disabled={d.busy !== null} onClick={() => d.decide("park", "", parkUntil)}>
                  {t("Park it")}
                </Button>
              </div>
            )}
            {d.error && (
              <p className="rv-error" role="alert">
                {d.error}
              </p>
            )}
            <button type="button" className="rv-dlnk" disabled={off} onClick={() => setMode(mode === "park" ? null : "park")}>
              {t("Not ready? Park it for later")}
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
              {t("Approve idea")}
            </Button>
          }
          secondary={
            <Button disabled={off || d.busy !== null} onClick={() => setMode(mode === "reject" ? null : "reject")}>
              {t("Send back…")}
            </Button>
          }
        />
      }
    />
  );
}

function MergeReview({ need, readOnly, index, total, onDone, onClose }: DecisionPanelProps) {
  const action = need.action as HomeNeed["action"] & { type: "merge" };
  const block = useWriteBlock(readOnly);
  const t = useTranslate();
  const [sendBack, setSendBack] = useState(false);
  const [takeBack, setTakeBack] = useState(false);
  const issue = useIssue(action.issue);
  const a = useApproveHead(action.pr, action.sha && action.sha.trim() ? action.sha : null);
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
  const off = !!block || !m.head || a.member;
  // One line per distinct error: a repeat of the same text shows once.
  const errors = [...new Set([a.error, m.error].filter((t): t is string => !!t))];
  return (
    <Shell
      need={need}
      verb="Approve change"
      title={issue?.title ?? need.shortTitle ?? "Publish a change"}
      subtitle={`${t("Ready")} ${waiting(need, t)}`}
      body={
        <>
          {lead && <p className="rv-lead">{lead}</p>}
          {size && (
            <>
              <h4 className="rv-h">{t("What changes")}</h4>
              <p className="rv-note">{size}</p>
            </>
          )}
          <Ticks items={ticks} />
          {!m.head && <p className="rv-error">{t("This has no checked version to approve. It needs a fresh review.")}</p>}
          {sendBack && (
            <SendBack
              label="Send back"
              placeholder="What should change before this goes out?"
              busy={m.busy === "decline"}
              onSend={m.decline}
            />
          )}
          {a.approved && (
            <>
              <p className="rv-note">{a.revocable ? t("Approved.") : t("Approved from your terminal.")}</p>
              {a.revocable && !block && (
                <button type="button" className="rv-dlnk" disabled={a.busy !== null} onClick={() => setTakeBack((t) => !t)}>
                  {t("Take my approval back")}
                </button>
              )}
              {a.revocable && !block && takeBack && (
                <SendBack
                  label="Take it back"
                  placeholder="Why are you taking it back?"
                  busy={a.busy === "revoke"}
                  onSend={a.revoke}
                />
              )}
            </>
          )}
          {a.revoked && <p className="rv-note">{t("Your approval was taken back. A new version needs a fresh approval.")}</p>}
          {a.member && <p className="rv-note">{t("Only the board's owner can approve or publish.")}</p>}
          {errors.map((text) => (
            <p key={text} className="rv-error" role="alert">
              {text}
            </p>
          ))}
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
            <Button variant="primary" loading={m.busy === "merge"} disabled={off || m.busy !== null} onClick={() => { a.clearError(); m.merge(); }}>
              {m.busy === "merge" ? t("Publishing…") : t("Publish")}
            </Button>
          }
          secondary={
            <>
              {a.available && !a.approved && !a.revoked && (
                <Button
                  disabled={!!block || a.member || a.busy !== null || m.busy !== null}
                  loading={a.busy === "approve"}
                  title={block ? t(READ_ONLY_COPY) : undefined}
                  onClick={() => { m.clearError(); a.approve(); }}
                >
                  {t("Approve this version")}
                </Button>
              )}
              <Button disabled={!!block || m.busy !== null} onClick={() => { setSendBack((s) => !s); m.clearError(); }}>
                Send back…
              </Button>
            </>
          }
        />
      }
    />
  );
}

/** One reading item's decision; the shared detail keys it by the item so each starts fresh. */
export default function DecisionPanel(props: DecisionPanelProps) {
  const t = props.need.action.type;
  if (t === "plan") return <PlanReview {...props} />;
  if (t === "idea") return <IdeaReview {...props} />;
  if (t === "merge") return <MergeReview {...props} />;
  return null;
}
