import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { useWriteBlock } from "../auth/WriteGate";
import { resources } from "../../lib/resources";
import { carriesSecret } from "../app-shell/chat/directive";
import { sendToMaster } from "./send";
import { newMessageId, type FormStatus, type ThreadEntry } from "./thread";
import {
  ACTION_MARK,
  CHECK,
  INPUTS,
  MAX_DEPTH,
  answeredData,
  asList,
  asText,
  binding,
  displayed,
  failedCheck,
  parse,
  resolve,
  setAt,
  walk,
  type Comp,
  type Data,
} from "./formModel";
import { TurnCard, type CardDensity, type CardTone } from "../../ui/TurnCard";

/**
 * CAD-2285: the master's A2UI form, drawn from the pilot catalog only.
 * The daemon has already checked the envelope; this renders what it knows
 * and shows the entry's own fallback text for anything else. The answer is
 * the operator's chat message citing the form (`refs` kind `surface`); the
 * form grants nothing beyond that message.
 */

const DRAFT = (seq: number) => `cadence.form.${seq}`;
function clock(at: string | null): string {
  if (!at) return "";
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
}

function readDraft(seq: number): Data | null {
  try {
    const raw = window.localStorage.getItem(DRAFT(seq));
    const v: unknown = raw ? JSON.parse(raw) : null;
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Data) : null;
  } catch {
    return null;
  }
}
function writeDraft(seq: number, data: Data | null): void {
  try {
    if (data) window.localStorage.setItem(DRAFT(seq), JSON.stringify(data));
    else window.localStorage.removeItem(DRAFT(seq));
  } catch {
    /* a blocked store only loses the draft */
  }
}

export default function AgentForm({
  entry,
  status,
  density = "full",
}: {
  entry: ThreadEntry;
  status: FormStatus;
  /** CAD-2440: the thread's density; the assistant panel is compact. */
  density?: CardDensity;
}) {
  const t = useTranslate();
  const block = useWriteBlock(false);
  const surface = useMemo(() => parse(entry), [entry]);
  const open = status.state === "open" || status.state === "failed";
  const [local, setLocal] = useState<Data>(() => (surface ? { ...surface.data, ...(open ? readDraft(entry.seq) ?? {} : {}) } : {}));
  const answer = status.state === "sent" ? status.answer : null;
  // A sent form shows its answer, also after a reload or an answer from another device.
  const answered = useMemo(
    () => (surface && answer !== null ? answeredData(walk(surface.comps), surface.data, answer) : null),
    [surface, answer],
  );
  const data = answered?.data ?? local;
  const [errors, setErrors] = useState<Record<string, string>>({});
  const message = useRef(newMessageId());
  const sent = useRef(false);
  // A sent or closed form keeps no draft on this device.
  useEffect(() => {
    if (status.state === "sent" || status.state === "closed") writeDraft(entry.seq, null);
    // Open again (a failed answer was discarded): Send works again.
    if (status.state === "open") sent.current = false;
  }, [status.state, entry.seq]);
  const root = useRef<HTMLElement>(null);

  if (!surface) {
    return (
      <div className="agentform-fallback text-secondary text-ink-300 whitespace-pre-wrap break-words" data-kind="form-fallback">
        {entry.text}
      </div>
    );
  }
  const order = walk(surface.comps);
  const title = order.find((c) => c.component === "Text" && typeof c.text === "string");
  const disabled = status.state !== "open" || block !== null;

  const update = (path: string | null, value: unknown) => {
    if (path === null || disabled) return;
    const next = setAt(data, path, value);
    setLocal(next);
    writeDraft(entry.seq, next);
    setErrors({});
  };

  const label = (c: Comp): string => asText(resolve(c.label, data));
  const shown = (c: Comp): string => {
    const v = resolve(c.value, data);
    if (c.component === "CheckBox") return v === true ? t("Yes") : t("No");
    if (c.component === "ChoicePicker") {
      const opts = Array.isArray(c.options) ? (c.options as { label?: unknown; value?: unknown }[]) : [];
      const names = (displayed(c, data) as string[]).map((x) => asText(resolve(opts.find((o) => o.value === x)?.label, data)) || x);
      return names.join(", ") || "—";
    }
    return asText(v) || "—";
  };

  const submit = (button: Comp) => {
    // One send per form: a double click must not post a second, different action.
    if (sent.current) return;
    const bad: Record<string, string> = {};
    for (const c of order) {
      const why = failedCheck(c, data);
      if (why) bad[c.id] = why;
    }
    if (Object.keys(bad).length > 0) {
      setErrors(bad);
      const first = order.find((c) => bad[c.id]);
      if (first) root.current?.querySelector<HTMLElement>(`[data-form-field="${CSS.escape(first.id)}"] :is(input,textarea,button)`)?.focus();
      return;
    }
    const event = ((button.action as Record<string, unknown> | undefined)?.event ?? {}) as { name?: unknown; context?: unknown };
    const context: Data = {};
    // Each context binding names an input's value (the daemon checks it);
    // send what that input shows, never the raw data behind it.
    const inputs = order.filter((c) => INPUTS.has(c.component));
    if (event.context && typeof event.context === "object") {
      for (const [k, v] of Object.entries(event.context as Data)) {
        const input = inputs.find((c) => binding(c.value) !== null && binding(c.value) === binding(v));
        context[k] = input ? displayed(input, data) : null;
      }
    }
    // One line per field, whatever the values hold.
    const lines = inputs.map((c) => `${label(c) || c.id}: ${shown(c)}`.replace(/\s*[\r\n\u2028\u2029]+\s*/g, " "));
    const action = {
      version: "1.0",
      action: { name: event.name, surfaceId: surface.id, sourceComponentId: button.id, timestamp: new Date().toISOString(), context },
    };
    const json = JSON.stringify(action);
    // The daemon already refuses reserved keys; never send one regardless.
    if (carriesSecret(json)) {
      setErrors({ [button.id]: "This answer carries a reserved key and was not sent." });
      return;
    }
    // Host wording, not the agent's title: the lines echo the form's own labels.
    const text = [`Form answer (${surface.id}):`, ...lines].join("\n") + ACTION_MARK + json;
    writeDraft(entry.seq, null);
    sent.current = true;
    // The Home conversation the composer sends to, so the answer lands beside the form.
    const conversation = resources.masterThread.get().data?.conversation;
    void sendToMaster(text, message.current, [{ kind: "surface", id: String(entry.seq) }], undefined, conversation);
  };

  const field = (c: Comp, body: ReactNode, legend = true): ReactNode => {
    const err = errors[c.id];
    const id = `form-${entry.seq}-${c.id}`;
    return (
      <fieldset key={c.id} className="agentform-field" data-form-field={c.id} data-invalid={err ? "" : undefined} aria-describedby={err ? `${id}-err` : undefined}>
        {legend && label(c) && <legend>{label(c)}</legend>}
        {body}
        {err && (
          <p className="text-micro text-fail" id={`${id}-err`} role="alert">
            {err === CHECK ? t(err) : err}
          </p>
        )}
      </fieldset>
    );
  };

  // The daemon checked the tree; the depth guard keeps a bad payload finite.
  const render = (c: Comp | undefined, depth = 1): ReactNode => {
    if (!c || depth > MAX_DEPTH) return null;
    const kids = (list: unknown) =>
      (Array.isArray(list) ? list : []).map((k) => render(typeof k === "string" ? surface.comps.get(k) : undefined, depth + 1));
    switch (c.component) {
      case "Card":
        return <div key={c.id} className="agentform-col">{render(surface.comps.get(asText(c.child)), depth + 1)}</div>;
      case "Column":
        return <div key={c.id} className="agentform-col">{kids(c.children)}</div>;
      case "Row":
        return <div key={c.id} className="agentform-row">{kids(c.children)}</div>;
      case "Divider":
        return <hr key={c.id} className="agentform-hr" />;
      case "Text": {
        const s = asText(resolve(c.text, data));
        if (c === title) return <h3 key={c.id}>{s}</h3>;
        return <p key={c.id} className={c.variant === "caption" ? "agentform-lead" : "agentform-text"}>{s}</p>;
      }
      case "Button": {
        const words = asText(resolve(surface.comps.get(asText(c.child))?.text, data)) || t("Send answer");
        const cls = c.variant === "primary" ? "btn btn-sm btn-primary" : c.variant === "borderless" ? "lnk text-label" : "btn btn-sm";
        if (!open) return null;
        return (
          <div key={c.id} className="agentform-foot">
            {status.state === "failed" ? (
              <>
                {/* One Retry: the answer's own row below resends the same message. */}
                <span className="text-micro text-fail" role="alert">{status.pending.error ?? t("Not sent")}</span>
                <span className="text-micro text-ink-500">{t("Use Retry on your message below.")}</span>
              </>
            ) : (
              <>
                <button type="button" className={cls} onClick={() => submit(c)} disabled={disabled}>{words}</button>
                <span className="text-micro text-ink-500">{block ?? t("Goes to the assistant as your message.")}</span>
                {errors[c.id] && <span className="text-micro text-fail" role="alert">{t(errors[c.id])}</span>}
              </>
            )}
          </div>
        );
      }
      case "CheckBox":
        return field(
          c,
          <label className="agentform-check">
            <input type="checkbox" checked={resolve(c.value, data) === true} disabled={disabled} onChange={(e) => update(binding(c.value), e.target.checked)} />
            {label(c)}
          </label>,
          false,
        );
      case "ChoicePicker": {
        const opts = Array.isArray(c.options) ? (c.options as { label?: unknown; value?: unknown }[]) : [];
        const one = c.variant === "mutuallyExclusive";
        const picked = asList(resolve(c.value, data));
        const pick = (v: string) => update(binding(c.value), one ? [v] : picked.includes(v) ? picked.filter((x) => x !== v) : [...picked, v]);
        // `displayStyle` is a hint: both chips and checkbox draw as the board's chips.
        const keys = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
          if (!one || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
          e.preventDefault();
          const next = (i + (e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 1) + opts.length) % opts.length;
          pick(asText(opts[next].value));
          (e.currentTarget.parentElement?.children[next] as HTMLElement | undefined)?.focus();
        };
        const focusIndex = Math.max(0, opts.findIndex((o) => picked.includes(asText(o.value))));
        return field(
          c,
          <div className="agentform-opts" role={one ? "radiogroup" : "group"} aria-label={label(c) || undefined}>
            {opts.map((o, i) => {
              const v = asText(o.value);
              const on = picked.includes(v);
              return (
                <button
                  key={v}
                  type="button"
                  className="chip chip-sans chip-btn"
                  role={one ? "radio" : undefined}
                  aria-checked={one ? on : undefined}
                  aria-pressed={one ? undefined : on}
                  tabIndex={one ? (i === focusIndex ? 0 : -1) : undefined}
                  disabled={disabled}
                  onClick={() => pick(v)}
                  onKeyDown={(e) => keys(e, i)}
                >
                  {asText(resolve(o.label, data)) || v}
                </button>
              );
            })}
          </div>,
        );
      }
      case "TextField": {
        const v = asText(resolve(c.value, data));
        const set = (s: string) => update(binding(c.value), s);
        const ph = asText(resolve(c.placeholder, data)) || undefined;
        return field(
          c,
          c.variant === "longText" ? (
            <textarea className="field agentform-input" rows={3} value={v} placeholder={ph} disabled={disabled} aria-label={label(c) || undefined} onChange={(e) => set(e.target.value)} />
          ) : (
            <input className="field agentform-input" type={c.variant === "number" ? "number" : "text"} value={v} placeholder={ph} disabled={disabled} aria-label={label(c) || undefined} onChange={(e) => set(e.target.value)} />
          ),
        );
      }
      case "DateTimeInput": {
        const kind = c.enableDate === true && c.enableTime === true ? "datetime-local" : c.enableTime === true ? "time" : "date";
        return field(
          c,
          <input className="field num agentform-input" type={kind} value={asText(resolve(c.value, data))} disabled={disabled} aria-label={label(c) || undefined} onChange={(e) => update(binding(c.value), e.target.value)} />,
        );
      }
      default:
        return null;
    }
  };

  const chip: { tone: CardTone; text: string } =
    status.state === "sent"
      ? { tone: "ok", text: `✓ ${t("Sent")}${clock(status.at) ? ` · ${clock(status.at)}` : ""}` }
      : status.state === "closed"
        ? { tone: "muted", text: t("Closed") }
        : status.state === "failed"
          ? { tone: "fail", text: t("Not sent") }
          : status.state === "sending"
            ? { tone: "muted", text: t("Sending…") }
            : { tone: "accent", text: t("Waiting for you") };

  // CAD-2440: the card system's region shape. Header, fields, notes and
  // every hook are the form's own; only the frame is shared.
  return (
    <TurnCard
      ref={root}
      kind="form"
      density={density}
      className="card agentform"
      bodyClassName="agentform-body"
      data-kind="form"
      data-form-state={status.state}
      label={`${t("Form from the assistant")}: ${title ? asText(title.text) : ""}`}
      icon={
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" aria-hidden="true">
          <rect x="2.75" y="2" width="10.5" height="12" rx="2" />
          <path d="M5.25 5.5h5.5M5.25 8h5.5M5.25 10.5h3" />
        </svg>
      }
      kindLabel={t("Form")}
      status={{ text: chip.text, tone: chip.tone, attrs: { "data-form-status": status.state } }}
    >
      {render(surface.comps.get("root"))}
      {status.state === "closed" && <p className="text-micro text-ink-500">{t("A newer message closed this form.")}</p>}
      {answered && !answered.known && <p className="text-micro text-ink-500">{t("Sent. Your answer is the message below.")}</p>}
    </TurnCard>
  );
}
