import { useEffect, useRef, useState, type ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { useWriteBlock } from "../auth/WriteGate";
import { READ_ONLY_COPY } from "../home/AnswerForm";
import { useTranslate } from "../../lib/localeText";
import { ageText, failureReason, isolate, outcomeOf, sentFields, stagedDomain, stagedDomainLabel, type StagedChange, type StagedDomain, type StagedWrite } from "./stagedWrite";
import type { DecisionNotice, DecisionsState, DecisionsStore, Verb } from "./useDecisions";
import "./decisions.css";

type Say = (english: string) => string;

/** The record link is the API's word; only a same-origin path or an https link is followed. */
function safeLink(href: string | null): string | null {
  if (href === null) return null;
  if (href.startsWith("/") && !href.startsWith("//")) return href;
  return href.startsWith("https://") ? href : null;
}

/** Text from the API is isolated: a bidi override in it cannot reach the words around it. */
const B = ({ children }: { children: ReactNode }) => <bdi>{typeof children === "string" ? isolate(children) : children}</bdi>;

/** Fill `{name}` in a translated template with a node. */
function fill(template: string, values: Record<string, ReactNode>): ReactNode[] {
  return template.split(/(\{\w+\})/).map((part, i) => {
    const key = /^\{(\w+)\}$/.exec(part)?.[1];
    return key !== undefined && key in values ? <span key={i}>{values[key]}</span> : part;
  });
}

/** A value shown as data: wrapped, never cut, never styled as UI. `empty`: nothing was there to clear. */
function Data({ v, t, empty = false }: { v: string | null | undefined; t: Say; empty?: boolean }) {
  if (v === null) return <em className="dx-none">{empty ? t("Empty") : t("(cleared)")}</em>;
  if (v === "") return <em className="dx-none">{t("(empty text)")}</em>;
  if (v === undefined) return null;
  return (
    <span className="dx-v">
      <B>{v}</B>
    </span>
  );
}

function ChangeRow({ change, update, create, t }: { change: StagedChange; update: boolean; create: boolean; t: Say }) {
  // A create has nothing to compare with: its fields are just the values.
  const compared = change.kind !== "added" || update;
  return (
    <div className="dx-row">
      <dt>
        <B>{change.label.trim() === "" ? t("(unnamed field)") : change.label}</B>
      </dt>
      <dd>
        {compared && (
          <>
            {change.before === null || change.before === undefined ? (
              <span className="dx-was">—</span>
            ) : (
              <span className="dx-was">
                <Data v={change.before} t={t} />
              </span>
            )}{" "}
            <span aria-hidden="true">→</span>{" "}
          </>
        )}
        <span className="dx-now">{change.after === undefined ? "—" : <Data v={change.after} t={t} empty={create && change.kind === "added"} />}</span>
      </dd>
    </div>
  );
}

/** FSI…PDI: the title cannot bleed into the sentence around it. */
const iso = (text: string) => `\u2068${isolate(text)}\u2069`;

export function noticeText(n: DecisionNotice, t: Say): string {
  switch (n.outcome) {
    case "applied":
      return `${t("Applied")} · ${iso(n.title)}`;
    case "denied":
      return `${t("Denied")} · ${iso(n.title)} · ${t("nothing ran")}`;
    case "failed":
      return t("{title} was approved but did not apply: {reason}. See Updates.")
        .replace("{title}", iso(n.title))
        .replace("{reason}", t(n.reason ?? "the reason is not recognised"));
    case "unconfirmed":
      return t("{title}: the result could not be confirmed. See Updates.").replace("{title}", iso(n.title));
    case "still-waiting":
      return t("It is still waiting. Nothing was recorded, so you can decide again.");
    default:
      return t("Already decided elsewhere. See Updates.");
  }
}

type DetailTabId = "approvals" | "overview" | "evidence" | "audit";

/** The Approvals tab body for one write. Keyed by write.id, so its disclosure states are its own. */
function ApprovalsTab({
  write,
  notice,
  state,
  pending,
  off,
  denyOpen,
  onToggleDenyOpen,
  denyReason,
  onDenyReasonChange,
  t,
}: {
  write: StagedWrite;
  notice: DecisionNotice | null;
  state: ReactNode;
  pending: boolean;
  off: boolean;
  denyOpen: boolean;
  onToggleDenyOpen: () => void;
  denyReason: string;
  onDenyReasonChange: (next: string) => void;
  t: Say;
}) {
  // An incomplete summary opens the raw call: that is the source of truth.
  const [raw, setRaw] = useState(!write.changesComplete);
  // No comparison was made when the summary is incomplete: `added` then means "value sent".
  const update = write.target !== null && write.changesComplete;
  const heading = update ? t("Changes compared with the current record") : write.target !== null ? t("Values sent") : t("Fields");
  const name = write.origin?.label ?? t("The assistant");
  const unchanged = write.unchangedCount;
  // A create lists what the call sent; the store's own defaults fold into one closed disclosure.
  const sent = update ? null : sentFields(write);
  const main = sent === null ? write.changes : write.changes.filter((c) => sent.has(c.field));
  const folded = sent === null || main.length === 0 ? [] : write.changes.filter((c) => !sent.has(c.field));
  const shown = folded.length === 0 ? write.changes : main;
  const [defaults, setDefaults] = useState(false);
  return (
    <div className="wi-panel dx-panel">
      {notice && (
        <p className={`issue-peek-note ${notice.outcome === "applied" || notice.outcome === "denied" ? "ok" : "warn"} m-0`} role="status">
          {noticeText(notice, t)}
        </p>
      )}
      {state}
      {!write.changesComplete && (
        <p className="issue-peek-note warn m-0" role="status">
          {t("This summary may be incomplete. Check the raw call.")}
        </p>
      )}
      {(write.changes.length > 0 || write.changesComplete) && (
        <section aria-label={heading}>
          <h4 className="rv-h">{heading}</h4>
          {write.changes.length > 0 ? (
            <>
              <dl className="dx-chg">
                {shown.map((c) => (
                  <ChangeRow key={`${c.kind}:${c.field}`} change={c} update={update} create={write.target === null} t={t} />
                ))}
              </dl>
              {folded.length > 0 && (
                <>
                  <button type="button" className="rv-dlnk dx-rawbtn dx-defaults" aria-expanded={defaults} onClick={() => setDefaults((d) => !d)}>
                    {(folded.length === 1 ? t("{n} default also set") : t("{n} defaults also set")).replace("{n}", String(folded.length))}
                  </button>
                  {defaults && (
                    <dl className="dx-chg dx-folded">
                      {folded.map((c) => (
                        <ChangeRow key={`${c.kind}:${c.field}`} change={c} update={update} create={write.target === null} t={t} />
                      ))}
                    </dl>
                  )}
                </>
              )}
            </>
          ) : (
            <p className="rv-note">{t("No fields change.")}</p>
          )}
          {unchanged > 0 && (
            <p className="rv-note">
              {(unchanged === 1 ? t("{n} field unchanged") : t("{n} fields unchanged")).replace("{n}", String(unchanged))}
            </p>
          )}
        </section>
      )}
      {write.reason !== null && (
        <div>
          <p className="rv-note dx-qh">{fill(t("{name}'s note · not verified"), { name: <B>{name}</B> })}</p>
          <blockquote className="dx-quote">
            <B>{write.reason}</B>
          </blockquote>
        </div>
      )}
      {write.consent !== null && (
        <p className="rv-note dx-consent">
          <span>{t("Consent supplied with this write · reported, not verified")}</span>
          {write.consent.map(([k, v]) => (
            <span key={k} className="dx-kv">
              <B>{k.replace(/_/g, " ")}</B>: <B>{v}</B>
            </span>
          ))}
        </p>
      )}
      {write.effects.length > 0 && (
        <p className="rv-note dx-eff">
          {write.effects.map((e, i) => (
            <span key={i}>
              {i > 0 && " · "}
              <B>{e}</B>
            </span>
          ))}
        </p>
      )}
      {pending && !off && (
        <div>
          <button type="button" className="rv-dlnk dx-rawbtn dx-deny-toggle" aria-expanded={denyOpen} onClick={onToggleDenyOpen}>
            {denyOpen ? t("Hide deny note") : t("Add deny note (optional)")}
          </button>
          {denyOpen && (
            <div className="dx-deny-box">
              <textarea
                value={denyReason}
                onChange={(e) => onDenyReasonChange(e.target.value)}
                maxLength={500}
                rows={2}
                placeholder={t("Why are you denying this request? (optional)")}
                aria-label={t("Deny note (optional)")}
              />
            </div>
          )}
        </div>
      )}
      <div>
        <button type="button" className="rv-dlnk dx-rawbtn" aria-expanded={raw} onClick={() => setRaw((r) => !r)}>
          {raw ? t("Hide raw") : t("Show raw")}
        </button>
        {raw && (
          <pre className="rv-tech dx-raw" tabIndex={0}>
            <B>{write.raw}</B>
          </pre>
        )}
      </div>
    </div>
  );
}

function OverviewTab({ write, domain, via, t }: { write: StagedWrite; domain: StagedDomain; via: string; t: Say }) {
  const name = write.origin?.label ?? t("The assistant");
  const originText =
    write.origin === null
      ? t("the assistant")
      : `${write.origin.label} (${write.origin.kind === "agent" || write.origin.kind === "person" ? t(write.origin.kind) : write.origin.kind})`;
  return (
    <div className="wi-panel dx-panel">
      <section className="dx-sec" aria-label={t("Overview")}>
        <h4 className="rv-h">{t("What this request does")}</h4>
        <div className="dx-kv-card">
          <dl className="dx-kv-list">
            <div className="dx-kv-line">
              <dt>{t("Domain")}</dt>
              <dd>{t(stagedDomainLabel(domain))}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Kind")}</dt>
              <dd>{write.kind === "create_issue" ? t("Issue proposal") : t("App write")}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Surface")}</dt>
              <dd>{write.app ? <B>{write.app}</B> : t("Workspace board")}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Target record")}</dt>
              <dd>
                {write.target ? (
                  <>
                    <B>{write.target.label}</B>
                    {write.target.id ? (
                      <>
                        {" · "}
                        <span className="dx-id">
                          <B>{write.target.id}</B>
                        </span>
                      </>
                    ) : null}
                  </>
                ) : (
                  t("New item")
                )}
              </dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Asked by")}</dt>
              <dd>{write.origin === null ? t("the assistant") : <B>{originText}</B>}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Channel")}</dt>
              <dd>
                <B>{via}</B>
              </dd>
            </div>
          </dl>
        </div>
      </section>
      {write.reason !== null && (
        <section className="dx-sec">
          <h4 className="rv-h">{fill(t("{name}'s note · not verified"), { name: <B>{name}</B> })}</h4>
          <blockquote className="dx-quote">
            <B>{write.reason}</B>
          </blockquote>
        </section>
      )}
    </div>
  );
}

function EvidenceTab({ write, t }: { write: StagedWrite; t: Say }) {
  // Incomplete summaries open the raw call automatically; complete ones keep
  // raw fields (such as expected_revision) behind Show raw per CAD-2186.
  const [raw, setRaw] = useState(!write.changesComplete);
  return (
    <div className="wi-panel dx-panel">
      <section className="dx-sec" aria-label={t("Evidence")}>
        <h4 className="rv-h">{t("Recorded comparison")}</h4>
        <div className="dx-kv-card">
          <dl className="dx-kv-list">
            <div className="dx-kv-line">
              <dt>{t("Fields listed")}</dt>
              <dd>{String(write.changes.length)}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Unchanged fields")}</dt>
              <dd>{String(write.unchangedCount)}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Summary coverage")}</dt>
              <dd>{write.changesComplete ? t("Complete") : t("Incomplete — check raw call")}</dd>
            </div>
          </dl>
        </div>
      </section>
      {(write.consent !== null || write.effects.length > 0) && (
        <section className="dx-sec">
          {write.consent !== null && (
            <p className="rv-note dx-consent">
              <span>{t("Consent supplied with this write · reported, not verified")}</span>
              {write.consent.map(([k, v]) => (
                <span key={k} className="dx-kv">
                  <B>{k.replace(/_/g, " ")}</B>: <B>{v}</B>
                </span>
              ))}
            </p>
          )}
          {write.effects.length > 0 && (
            <p className="rv-note dx-eff">
              {write.effects.map((e, i) => (
                <span key={i}>
                  {i > 0 && " · "}
                  <B>{e}</B>
                </span>
              ))}
            </p>
          )}
        </section>
      )}
      <section className="dx-sec">
        <h4 className="rv-h">{t("Exact staged payload")}</h4>
        <div>
          <button type="button" className="rv-dlnk dx-rawbtn" aria-expanded={raw} onClick={() => setRaw((r) => !r)}>
            {raw ? t("Hide raw") : t("Show raw")}
          </button>
          {raw && (
            <pre className="rv-tech dx-raw" tabIndex={0}>
              <B>{write.raw}</B>
            </pre>
          )}
        </div>
      </section>
    </div>
  );
}

const isoOrDash = (ms: number) => (Math.abs(ms) <= 8.64e15 ? new Date(ms).toISOString() : "—");

function AuditTab({ write, t }: { write: StagedWrite; t: Say }) {
  const [rawCode, setRawCode] = useState(false);
  const outcome = outcomeOf(write);
  const receipt = write.result;
  return (
    <div className="wi-panel dx-panel">
      <section className="dx-sec" aria-label={t("Audit")}>
        <h4 className="rv-h">{t("Approval gate & receipt")}</h4>
        <div className="dx-kv-card">
          <dl className="dx-kv-list">
            <div className="dx-kv-line">
              <dt>{t("Request ID")}</dt>
              <dd>
                <span className="dx-id">
                  <B>{write.id}</B>
                </span>
              </dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Authority")}</dt>
              <dd>{t("Workspace owner only")}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Status")}</dt>
              <dd>{t(outcome)}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Staged")}</dt>
              <dd>{`${ageText(write.createdAt, t)} (${isoOrDash(write.createdAt)})`}</dd>
            </div>
            <div className="dx-kv-line">
              <dt>{t("Decided")}</dt>
              <dd>{write.decidedAt !== null ? `${ageText(write.decidedAt, t)} (${isoOrDash(write.decidedAt)})` : t("Waiting for owner")}</dd>
            </div>
            {write.decisionReason ? (
              <div className="dx-kv-line">
                <dt>{t("Decision note")}</dt>
                <dd>
                  <B>{write.decisionReason}</B>
                </dd>
              </div>
            ) : null}
            {receipt ? (
              <div className="dx-kv-line">
                <dt>{t("Execution receipt")}</dt>
                <dd>
                  {receipt.ticket ? (
                    <B>{receipt.ticket}</B>
                  ) : receipt.record?.id ? (
                    <B>{receipt.record.id}</B>
                  ) : receipt.ok === false ? (
                    `${t("Did not apply")}: ${t(failureReason(write))}`
                  ) : receipt.ok ? (
                    t("Applied")
                  ) : (
                    t("Did not apply")
                  )}
                </dd>
              </div>
            ) : null}
          </dl>
        </div>
        {receipt?.code && (
          <div>
            <button type="button" className="rv-dlnk dx-rawbtn" aria-expanded={rawCode} onClick={() => setRawCode((r) => !r)}>
              {rawCode ? t("Hide raw") : t("Show raw")}
            </button>
            {rawCode && (
              <pre className="rv-tech dx-raw" tabIndex={0}>
                <B>{receipt.code}</B>
              </pre>
            )}
          </div>
        )}
      </section>
      <p className="rv-note">{t("Approved writes run at most once as the workspace owner. Denials run nothing.")}</p>
    </div>
  );
}

/** How long a newly shown item ignores Approve and Deny. */
const ARM_MS = 800;

const TABBABLE = 'button:not([disabled]):not([tabindex="-1"]), a[href]:not([tabindex="-1"]), textarea:not([disabled]):not([tabindex="-1"]), input:not([disabled]):not([tabindex="-1"]), pre[tabindex="0"], [tabindex]:not([tabindex="-1"])';

/**
 * CAD-2186: one staged write in the shared DrawerShell, with the queue of
 * what is waiting. The shell is mounted once per opening: deciding advances
 * the queue by changing props, so the drawer never re-opens, and Show raw is
 * state inside the body, so opening it touches nothing else.
 */
export default function StagedWriteDetail({
  decisions,
  store,
  queue,
  at,
  readOnly,
  onAt,
  onClose,
}: {
  decisions: DecisionsState;
  store: DecisionsStore;
  /** The ids that were waiting when the detail opened ("n of m" does not recount). */
  queue: readonly string[];
  at: number;
  readOnly: boolean;
  onAt: (index: number) => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const block = useWriteBlock(readOnly);
  const id = queue[at] ?? "";
  const write = decisions.rows.find((r) => r.id === id) ?? null;
  const attempt = decisions.attempts.get(id);
  const busy = [...decisions.attempts.values()].find((a) => a.phase === "busy");
  const pending = write?.status === "pending";
  const lost = decisions.phase === "unavailable" || attempt?.phase === "access";
  // Our own decision leaves "pending" a moment before the queue advances.
  const mine = useRef<string | null>(null);
  // This drawer may reopen while its own request from a previous mount settles.
  const resumed = useRef<string | null>(attempt?.phase === "busy" ? id : null);
  const uncertain = attempt?.phase === "unconfirmed";
  // Unsure of our own decision wins over "decided elsewhere": Check again reports the real outcome.
  const elsewhere = !lost && !uncertain && !pending && mine.current !== id && resumed.current !== id;
  const idRef = useRef(id);
  idRef.current = id;
  const atRef = useRef(at);
  atRef.current = at;
  const [tab, setTab] = useState<DetailTabId>("approvals");
  const [denyOpen, setDenyOpen] = useState(false);
  const [denyReason, setDenyReason] = useState("");
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // A new item is a new subject: land on its title (the first mount is the shell's), and reset tab + deny note.
  const first = useRef(true);
  useEffect(() => {
    setTab("approvals");
    setDenyOpen(false);
    setDenyReason("");
    if (first.current) {
      first.current = false;
      return;
    }
    document.querySelector<HTMLElement>('.crm-drawer[data-drawer="staged"] .crm-drawer-title')?.focus();
  }, [id]);

  // Focus stays in the drawer until it closes (the shell is non-modal by default).
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const root = document.querySelector<HTMLElement>('.crm-drawer[data-drawer="staged"]');
      if (root === null) return;
      const items = Array.from(root.querySelectorAll<HTMLElement>(TABBABLE));
      if (items.length === 0) return;
      const active = document.activeElement;
      const onTitle = active instanceof HTMLElement && active.classList.contains("crm-drawer-title");
      if (!(active instanceof Node) || !root.contains(active)) {
        e.preventDefault();
        items[e.shiftKey ? items.length - 1 : 0].focus();
      } else if (e.shiftKey && (active === items[0] || onTitle)) {
        e.preventDefault();
        items[items.length - 1].focus();
      } else if (!e.shiftKey && active === items[items.length - 1]) {
        e.preventDefault();
        items[0].focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, []);

  /** The next item that is still waiting, else close. Nothing is decided by moving on. */
  const advance = () => {
    const s = store.currentDecisions();
    for (let j = atRef.current + 1; j < queue.length; j++) {
      if (s.pending.some((p) => p.id === queue[j])) {
        onAt(j);
        return;
      }
    }
    onClose();
  };

  const run = async (work: () => Promise<string>) => {
    mine.current = idRef.current;
    const result = await work();
    if (!alive.current) return;
    if (result === "decided") advance();
    else mine.current = null;
  };
  // A new item is not armed at once: a second click of a double-click that
  // decided the previous item must not land on this one.
  const armedAt = useRef(0);
  useEffect(() => {
    armedAt.current = Date.now() + ARM_MS;
  }, [id]);
  const toggleDenyOpen = () => {
    setDenyOpen((prev) => {
      if (prev) setDenyReason("");
      return !prev;
    });
  };
  const act = (verb: Verb, forId: string, clicks: number) => {
    if (clicks > 1 || Date.now() < armedAt.current) return;
    // The click is for the row that was on screen when the handler was made.
    if (forId !== idRef.current) return;
    if (block !== null || !pending || busy !== undefined || lost || uncertain) return;
    const reason = verb === "deny" && denyOpen ? denyReason : undefined;
    void run(() => store.decide(forId, verb, reason));
  };
  const check = () => {
    if (busy !== undefined) return;
    void run(() => store.checkDecision(idRef.current));
  };
  const skip = () => {
    if (busy !== undefined) return;
    store.clearNotice();
    advance();
  };

  const n = queue.length;
  const label = t("Decision");
  if (write === null) {
    // Gone from the list: nothing to approve here.
    return (
      <DrawerShell
        kind="staged"
        modal
        scrim="board"
        label={label}
        title={t("This request is no longer waiting")}
        tabs={[
          {
            id: "detail",
            label,
            panel: (
              <p className="issue-peek-note warn m-0" role="alert">
                {lost
                  ? t("Only the workspace owner can decide this request.")
                  : uncertain
                    ? t("We could not confirm the result. It may already have gone through, so do not approve again; check first.")
                    : t("Already decided elsewhere. See Updates.")}
              </p>
            ),
          },
        ]}
        foot={
          <div className="crm-drawer-actions">
            <button type="button" className="btn btn-sm btn-primary" onClick={lost ? onClose : uncertain ? check : advance}>
              {lost ? t("Close") : uncertain ? t("Check again") : t("Continue")}
            </button>
          </div>
        }
        onClose={onClose}
      />
    );
  }

  const state = lost ? (
    <p className="issue-peek-note err m-0" role="alert">
      {t("Only the workspace owner can decide this request.")}
    </p>
  ) : elsewhere || attempt?.phase === "elsewhere" ? (
    <p className="issue-peek-note warn m-0" role="alert">
      {t("This was already decided elsewhere. The list has been refreshed.")}
    </p>
  ) : uncertain ? (
    <p className="issue-peek-note warn m-0" role="alert">
      {t("We could not confirm the result. It may already have gone through, so do not approve again; check first.")}
    </p>
  ) : block !== null ? (
    <p className="issue-peek-note warn m-0" role="status">
      {t(READ_ONLY_COPY)}
    </p>
  ) : null;

  const via = write.origin === null ? t(store.conversation === null ? "in Home" : "in this conversation") : t("via {via}").replace("{via}", write.origin.via.toLowerCase() === "cli" ? "CLI" : write.origin.via);
  const link = safeLink(write.recordLink);
  const where = [write.app, write.target?.label].filter((p): p is string => !!p).join(" · ");
  const domain = stagedDomain(write);
  const off = block !== null || busy !== undefined || lost || uncertain || !pending;
  const spin = (verb: Verb) => busy?.phase === "busy" && busy.verb === verb && attempt?.phase === "busy";

  return (
    <DrawerShell
      kind="staged"
      modal
      scrim="board"
      label={label}
      title={<B>{write.title}</B>}
      subtitle={
        <>
          <B>{where}</B>
          {write.target?.id ? <span className="dx-id">{where ? " · " : ""}<B>{write.target.id}</B></span> : null}
          {link && (
            <>
              {" · "}
              <a className="lnk" href={link}>
                {t("Open record")} ↗
              </a>
            </>
          )}
          <div className="dx-asked">
            {t("Asked by")}{" "}
            <B>{write.origin === null ? t("the assistant") : `${write.origin.label} (${write.origin.kind === "agent" || write.origin.kind === "person" ? t(write.origin.kind) : write.origin.kind})`}</B>
            , <B>{via}</B> · {ageText(write.createdAt, t)}
          </div>
        </>
      }
      pills={<span className="dx-domain-chip" data-domain={domain}>{t(stagedDomainLabel(domain))}</span>}
      tab={tab}
      onTab={(next) => setTab(next as DetailTabId)}
      tabs={[
        {
          id: "approvals",
          label: t("Approvals"),
          panel: (
            <ApprovalsTab
              key={write.id}
              write={write}
              notice={decisions.notice}
              state={state}
              pending={pending}
              off={off}
              denyOpen={denyOpen}
              onToggleDenyOpen={toggleDenyOpen}
              denyReason={denyReason}
              onDenyReasonChange={setDenyReason}
              t={t}
            />
          ),
        },
        {
          id: "overview",
          label: t("Overview"),
          panel: <OverviewTab write={write} domain={domain} via={via} t={t} />,
        },
        {
          id: "evidence",
          label: t("Evidence"),
          panel: <EvidenceTab key={write.id} write={write} t={t} />,
        },
        {
          id: "audit",
          label: t("Audit"),
          panel: <AuditTab key={write.id} write={write} t={t} />,
        },
      ]}
      foot={
        <>
          <div className="crm-drawer-actions">
            {elsewhere || attempt?.phase === "elsewhere" ? (
              <button type="button" className="btn btn-sm btn-primary" onClick={advance}>
                {t("Continue")}
              </button>
            ) : uncertain ? (
              <button type="button" className="btn btn-sm btn-primary" aria-disabled={busy !== undefined || undefined} onClick={check}>
                {t("Check again")}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  aria-disabled={off || undefined}
                  aria-busy={spin("approve") || undefined}
                  title={block !== null ? t(READ_ONLY_COPY) : undefined}
                  onClick={(e) => act("approve", write.id, e.detail)}
                >
                  {spin("approve") ? (
                    <>
                      <span className="btn-spin" aria-hidden="true" />
                      {t("Approving…")}
                    </>
                  ) : (
                    t("Approve")
                  )}
                </button>
                <button
                  type="button"
                  className="btn btn-sm"
                  aria-disabled={off || undefined}
                  aria-busy={spin("deny") || undefined}
                  title={block !== null ? t(READ_ONLY_COPY) : undefined}
                  onClick={(e) => act("deny", write.id, e.detail)}
                >
                  {spin("deny") ? (
                    <>
                      <span className="btn-spin" aria-hidden="true" />
                      {t("Denying…")}
                    </>
                  ) : (
                    t("Deny")
                  )}
                </button>
              </>
            )}
            <button type="button" className="btn btn-sm btn-ghost" aria-disabled={busy !== undefined || undefined} onClick={skip}>
              {t("Not now")}
            </button>
          </div>
          <span className="crm-drawer-note" aria-live="polite">
            {t("{n} of {m}").replace("{n}", String(at + 1)).replace("{m}", String(n))}
          </span>
        </>
      }
      onClose={onClose}
    />
  );
}
