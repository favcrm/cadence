import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { useWriteBlock } from "../auth/WriteGate";
import { resources } from "../../lib/resources";
import { carriesSecret } from "../app-shell/chat/directive";
import { sendToMaster } from "./send";
import { newMessageId, type FormStatus, type ThreadEntry } from "./thread";

/**
 * CAD-2285: the master's A2UI form, drawn from the pilot catalog only.
 * The daemon has already checked the envelope; this renders what it knows
 * and shows the entry's own fallback text for anything else. The answer is
 * the operator's chat message citing the form (`refs` kind `surface`); the
 * form grants nothing beyond that message.
 */

type Comp = Record<string, unknown> & { id: string; component: string };
type Data = Record<string, unknown>;

/** Readable lines, then the A2UI action. ThreadView hides the action
 *  when it paints the operator's answer. */
export const ACTION_MARK = "\n\nA2UI action: ";

const DRAFT = (seq: number) => `cadence.form.${seq}`;
/** The daemon's tree limit (agent_form MAX_DEPTH). */
const MAX_DEPTH = 4;
/** The default check message; also the key it is translated by. */
const CHECK = "Check this field.";

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

/** The surface, or null when the payload is not the shape the daemon writes. */
function parse(entry: ThreadEntry): { id: string; comps: Map<string, Comp>; data: Data } | null {
  const create = (entry.payload?.a2ui as { createSurface?: unknown } | undefined)?.createSurface as
    | { surfaceId?: unknown; components?: unknown; dataModel?: unknown }
    | undefined;
  if (!create || typeof create.surfaceId !== "string" || !Array.isArray(create.components)) return null;
  const comps = new Map<string, Comp>();
  for (const c of create.components) {
    if (!c || typeof c !== "object" || typeof (c as Comp).id !== "string" || typeof (c as Comp).component !== "string") return null;
    comps.set((c as Comp).id, c as Comp);
  }
  if (!comps.has("root")) return null;
  const data = create.dataModel && typeof create.dataModel === "object" ? (create.dataModel as Data) : {};
  return { id: create.surfaceId, comps, data };
}

const binding = (v: unknown): string | null =>
  v && typeof v === "object" && typeof (v as { "@path"?: unknown })["@path"] === "string" ? (v as { "@path": string })["@path"] : null;

function getAt(data: Data, path: string): unknown {
  let cur: unknown = data;
  for (const key of path.split("/").slice(1)) {
    if (!cur || typeof cur !== "object") return undefined;
    cur = (cur as Data)[key.replace(/~1/g, "/").replace(/~0/g, "~")];
  }
  return cur;
}
function setAt(data: Data, path: string, value: unknown): Data {
  const keys = path.split("/").slice(1).map((k) => k.replace(/~1/g, "/").replace(/~0/g, "~"));
  const next: Data = { ...data };
  let cur = next;
  keys.forEach((k, i) => {
    if (i === keys.length - 1) cur[k] = value;
    else {
      const child = cur[k];
      cur[k] = child && typeof child === "object" && !Array.isArray(child) ? { ...(child as Data) } : {};
      cur = cur[k] as Data;
    }
  });
  return next;
}
const resolve = (v: unknown, data: Data): unknown => {
  const p = binding(v);
  return p === null ? v : getAt(data, p);
};
const asText = (v: unknown): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : "");
const asList = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : typeof v === "string" && v ? [v] : []);

/** The pilot checks: required, length, numeric, email. A failed rule yields its message. */
function failedCheck(c: Comp, data: Data): string | null {
  const rules = Array.isArray(c["@checks"]) ? (c["@checks"] as Record<string, unknown>[]) : [];
  for (const rule of rules) {
    const cond = rule?.condition as Record<string, unknown> | undefined;
    if (!cond) continue;
    let ok = true;
    const path = binding(cond);
    if (path !== null) ok = !!getAt(data, path);
    else {
      const args = (cond.args ?? {}) as Record<string, unknown>;
      const value = resolve(args.value, data);
      const min = typeof args.min === "number" ? args.min : null;
      const max = typeof args.max === "number" ? args.max : null;
      const empty = value === null || value === undefined || value === "" || (Array.isArray(value) && value.length === 0);
      switch (cond["@call"]) {
        case "required":
          ok = !empty;
          break;
        case "length": {
          const n = Array.isArray(value) ? value.length : asText(value).length;
          ok = (min === null || n >= min) && (max === null || n <= max);
          break;
        }
        case "numeric": {
          const n = Number(value);
          ok = asText(value) !== "" && Number.isFinite(n) && (min === null || n >= min) && (max === null || n <= max);
          break;
        }
        case "email":
          ok = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(asText(value));
          break;
      }
    }
    if (!ok) return typeof rule.message === "string" && rule.message ? rule.message : CHECK;
  }
  return null;
}

const INPUTS = new Set(["CheckBox", "ChoicePicker", "TextField", "DateTimeInput"]);

/** What an input shows: a CheckBox's tick, a picker's own options only, a field's text. */
function displayed(c: Comp, data: Data): unknown {
  const v = resolve(c.value, data);
  if (c.component === "CheckBox") return v === true;
  if (c.component === "ChoicePicker") {
    const options = (Array.isArray(c.options) ? (c.options as { value?: unknown }[]) : []).map((o) => o.value);
    const picked = asList(v).filter((x) => options.includes(x));
    return c.variant === "mutuallyExclusive" ? picked.slice(0, 1) : picked;
  }
  return asText(v);
}

/** Every component under root in reading order (the daemon checked it is a tree). */
function walk(comps: Map<string, Comp>): Comp[] {
  const out: Comp[] = [];
  const visit = (id: string, depth: number) => {
    const c = comps.get(id);
    if (!c || depth > MAX_DEPTH) return;
    out.push(c);
    const kids = Array.isArray(c.children) ? c.children : typeof c.child === "string" ? [c.child] : [];
    for (const k of kids) if (typeof k === "string") visit(k, depth + 1);
  };
  visit("root", 1);
  return out;
}

export default function AgentForm({ entry, status }: { entry: ThreadEntry; status: FormStatus }) {
  const t = useTranslate();
  const block = useWriteBlock(false);
  const surface = useMemo(() => parse(entry), [entry]);
  const open = status.state === "open" || status.state === "failed";
  const [data, setData] = useState<Data>(() => (surface ? { ...surface.data, ...(open ? readDraft(entry.seq) ?? {} : {}) } : {}));
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
    setData(next);
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

  const chip =
    status.state === "sent"
      ? { cls: "bg-ok/15 text-ok", text: `✓ ${t("Sent")}${clock(status.at) ? ` · ${clock(status.at)}` : ""}` }
      : status.state === "closed"
        ? { cls: "bg-ink-800 text-ink-400", text: t("Closed") }
        : status.state === "failed"
          ? { cls: "bg-fail/15 text-fail", text: t("Not sent") }
          : status.state === "sending"
            ? { cls: "bg-ink-800 text-ink-300", text: t("Sending…") }
            : { cls: "bg-accent/15 text-accent", text: t("Waiting for you") };

  return (
    <section
      ref={root}
      className="card agentform"
      data-kind="form"
      data-form-state={status.state}
      aria-label={`${t("Form from the assistant")}: ${title ? asText(title.text) : ""}`}
    >
      <div className="agentform-body">
        <div className="agentform-head">
          <span className="agentform-kind">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" aria-hidden="true">
              <rect x="2.75" y="2" width="10.5" height="12" rx="2" />
              <path d="M5.25 5.5h5.5M5.25 8h5.5M5.25 10.5h3" />
            </svg>
            {t("Form")}
          </span>
          <span className={`chip chip-sans ${chip.cls}`} data-form-status={status.state}>
            {chip.text}
          </span>
        </div>
        {render(surface.comps.get("root"))}
        {status.state === "closed" && <p className="text-micro text-ink-500">{t("A newer message closed this form.")}</p>}
      </div>
    </section>
  );
}
