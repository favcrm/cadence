/**
 * CAD-2285 / CAD-2346: the pure model of the master's A2UI form — parsing,
 * bindings, the pilot checks, and what a sent form shows. No React, so the
 * UI tests load it directly; `AgentForm.tsx` renders it.
 */
import type { ThreadEntry } from "./thread";

export type Comp = Record<string, unknown> & { id: string; component: string };
export type Data = Record<string, unknown>;

/** Readable lines, then the A2UI action. ThreadView hides the action
 *  when it paints the operator's answer. */
export const ACTION_MARK = "\n\nA2UI action: ";

/** The daemon's tree limit (agent_form MAX_DEPTH). */
export const MAX_DEPTH = 4;
/** The default check message; also the key it is translated by. */
export const CHECK = "Check this field.";

/** The surface, or null when the payload is not the shape the daemon writes. */
export function parse(entry: ThreadEntry): { id: string; comps: Map<string, Comp>; data: Data } | null {
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

export const binding = (v: unknown): string | null =>
  v && typeof v === "object" && typeof (v as { "@path"?: unknown })["@path"] === "string" ? (v as { "@path": string })["@path"] : null;

function getAt(data: Data, path: string): unknown {
  let cur: unknown = data;
  for (const key of path.split("/").slice(1)) {
    if (!cur || typeof cur !== "object") return undefined;
    cur = (cur as Data)[key.replace(/~1/g, "/").replace(/~0/g, "~")];
  }
  return cur;
}
export function setAt(data: Data, path: string, value: unknown): Data {
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
export const resolve = (v: unknown, data: Data): unknown => {
  const p = binding(v);
  return p === null ? v : getAt(data, p);
};
export const asText = (v: unknown): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : "");
export const asList = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : typeof v === "string" && v ? [v] : []);

/** The pilot checks: required, length, numeric, email. A failed rule yields its message. */
export function failedCheck(c: Comp, data: Data): string | null {
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
          // A blank optional field passes; only `required` refuses a blank (CAD-2346).
          ok = asText(value) === "" || /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(asText(value));
          break;
      }
    }
    if (!ok) return typeof rule.message === "string" && rule.message ? rule.message : CHECK;
  }
  return null;
}

export const INPUTS = new Set(["CheckBox", "ChoicePicker", "TextField", "DateTimeInput"]);

/** What an input shows: a CheckBox's tick, a picker's own options only, a field's text. */
export function displayed(c: Comp, data: Data): unknown {
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
export function walk(comps: Map<string, Comp>): Comp[] {
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

/**
 * CAD-2346: what a sent form shows, read from its answer, never the
 * surface's defaults. Every bound input starts blank; each key of the
 * submit Button's event context names an input's binding, and the
 * answer's A2UI action carries that key's value. `known` is false when
 * the answer's action cannot be read: the inputs then stay blank.
 */
export function answeredData(order: Comp[], base: Data, answer: string): { data: Data; known: boolean } {
  let data = base;
  for (const c of order) {
    const path = INPUTS.has(c.component) ? binding(c.value) : null;
    if (path === null) continue;
    data = setAt(data, path, c.component === "CheckBox" ? false : c.component === "ChoicePicker" ? [] : "");
  }
  const at = answer.lastIndexOf(ACTION_MARK);
  let sent: { context?: unknown; sourceComponentId?: unknown } | undefined;
  try {
    sent = at < 0 ? undefined : (JSON.parse(answer.slice(at + ACTION_MARK.length)) as { action?: typeof sent }).action;
  } catch {
    sent = undefined;
  }
  const context = sent?.context;
  // The button that sent the answer names its own bindings. Only an answer
  // that does not say which button sent it falls back to the first Button
  // with a context; one naming a button that has none is not read.
  const buttons = order.filter((c) => c.component === "Button" && (c.action as { event?: { context?: unknown } } | undefined)?.event?.context);
  const source = sent?.sourceComponentId;
  const button = source === undefined ? buttons[0] : buttons.find((c) => c.id === source);
  const bound = (button?.action as { event: { context: unknown } } | undefined)?.event.context;
  if (!context || typeof context !== "object" || Array.isArray(context) || !bound || typeof bound !== "object") return { data, known: false };
  for (const [key, ref] of Object.entries(bound as Data)) {
    const path = binding(ref);
    if (path !== null && Object.hasOwn(context, key)) data = setAt(data, path, (context as Data)[key]);
  }
  return { data, known: true };
}
