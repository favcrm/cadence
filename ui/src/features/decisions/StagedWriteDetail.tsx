import { useEffect, useRef, useState, type ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { useWriteBlock } from "../auth/WriteGate";
import { READ_ONLY_COPY } from "../home/AnswerForm";
import { useTranslate } from "../../lib/localeText";
import { ageText, isolate, sentFields, type StagedChange, type StagedWrite } from "./stagedWrite";
import { checkDecision, clearNotice, currentDecisions, decide, type DecisionNotice, type DecisionsState, type Verb } from "./useDecisions";
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

/** The body for one write. Keyed by the write, so its Show raw state is its own. */
function Body({ write, notice, state, t }: { write: StagedWrite; notice: DecisionNotice | null; state: ReactNode; t: Say }) {
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
      <div>
        <button type="button" className="rv-dlnk dx-rawbtn" aria-expanded={raw} onClick={() => setRaw((r) => !r)}>
          {raw ? t("Hide raw") : t("Show raw")}
        </button>
        {raw && (
          <pre className="rv-tech dx-raw" tabIndex={0}>
            {write.raw}
          </pre>
        )}
      </div>
    </div>
  );
}

/** How long a newly shown item ignores Approve and Deny. */
const ARM_MS = 800;

const TABBABLE = 'button:not([disabled]), a[href], pre[tabindex="0"], [tabindex]:not([tabindex="-1"])';

/**
 * CAD-2186: one staged write in the shared DrawerShell, with the queue of
 * what is waiting. The shell is mounted once per opening: deciding advances
 * the queue by changing props, so the drawer never re-opens, and Show raw is
 * state inside the body, so opening it touches nothing else.
 */
export default function StagedWriteDetail({
  decisions,
  queue,
  at,
  readOnly,
  onAt,
  onClose,
}: {
  decisions: DecisionsState;
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
  const uncertain = attempt?.phase === "unconfirmed";
  // Unsure of our own decision wins over "decided elsewhere": Check again reports the real outcome.
  const elsewhere = !lost && !uncertain && !pending && mine.current !== id;
  const idRef = useRef(id);
  idRef.current = id;
  const atRef = useRef(at);
  atRef.current = at;
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // A new item is a new subject: land on its title (the first mount is the shell's).
  const first = useRef(true);
  useEffect(() => {
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
    const s = currentDecisions();
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
  const act = (verb: Verb, forId: string, clicks: number) => {
    if (clicks > 1 || Date.now() < armedAt.current) return;
    // The click is for the row that was on screen when the handler was made.
    if (forId !== idRef.current) return;
    if (block !== null || !pending || busy !== undefined || lost || uncertain) return;
    void run(() => decide(forId, verb));
  };
  const check = () => {
    if (busy !== undefined) return;
    void run(() => checkDecision(idRef.current));
  };
  const skip = () => {
    if (busy !== undefined) return;
    clearNotice();
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

  const via = write.origin === null ? t("in Home") : t("via {via}").replace("{via}", write.origin.via.toLowerCase() === "cli" ? "CLI" : write.origin.via);
  const link = safeLink(write.recordLink);
  const where = [write.app, write.target?.label].filter((p): p is string => !!p).join(" · ");
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
      tabs={[{ id: "detail", label, panel: <Body key={write.id} write={write} notice={decisions.notice} state={state} t={t} /> }]}
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
