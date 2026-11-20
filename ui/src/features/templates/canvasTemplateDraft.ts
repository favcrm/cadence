import { onPrivateReadsPaused } from "../../lib/privateSession";
import type { HostedTemplateReviewTarget } from "./HostedTemplateReview";

export interface ExtractedPlaceholders { scalars: string[]; itemFields: string[] }
export interface CanvasTemplatePrefill { name: string; html: string; specJson: string }
export interface ParameterizedTemplateHtml { html: string; scalars: string[]; itemFields: string[]; autoParameterized: boolean }

const RESERVED = new Set("items subtotal discount tax total currency tax_rate discount_rate has_unresolved_pricing kind".split(" "));

export function extractHtmlPlaceholders(html: string): ExtractedPlaceholders {
  const scalars: string[] = [], seenS = new Set<string>(), itemFields: string[] = [], seenI = new Set<string>();
  for (const m of html.matchAll(/\{\{\s*([a-zA-Z_][a-zA-Z0-9_.]*)\s*\}\}/g)) {
    const t = m[1];
    if (!t) continue;
    if (t.startsWith("item.")) {
      const f = t.slice(5);
      if (f && f !== "line_total" && !seenI.has(f)) { seenI.add(f); itemFields.push(f); }
    } else if (!t.includes(".") && !RESERVED.has(t) && !seenS.has(t)) { seenS.add(t); scalars.push(t); }
  }
  return { scalars, itemFields };
}

function replaceOutsideTags(frag: string, target: string, rep: string): { text: string; changed: boolean } {
  const re = new RegExp(target.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g");
  let changed = false;
  const text = frag.split(/(<[^>]+>)/g).map((p) =>
    p.startsWith("<") && p.endsWith(">") ? p : p.replace(re, () => { changed = true; return rep; }),
  ).join("");
  return { text, changed };
}

export function parameterizeHtmlForTemplate(html: string, opts?: { customerLiteral?: string }): ParameterizedTemplateHtml {
  let next = html, auto = false;
  const lit = opts?.customerLiteral?.trim() ?? "";
  if (lit) {
    const bm = /<body\b[^>]*>/i.exec(next);
    const at = bm?.index !== undefined ? bm.index + bm[0].length : 0;
    const r = replaceOutsideTags(next.slice(at), lit, "{{ customer }}");
    if (r.changed) { next = `${next.slice(0, at)}${r.text}`; auto = true; }
  }
  let ex = extractHtmlPlaceholders(next);
  if (ex.scalars.length === 0) {
    const slot = '<div class="cadence-template-meta"><p>{{ customer }}</p></div>';
    next = /<body\b[^>]*>/i.test(next) ? next.replace(/<body\b[^>]*>/i, (t) => `${t}\n${slot}`) : `${slot}\n${next}`;
    auto = true; ex = extractHtmlPlaceholders(next);
  }
  return { html: next, scalars: ex.scalars, itemFields: ex.itemFields, autoParameterized: auto };
}

function makeSignal<T>() {
  let pending: T | null = null, sub = false;
  const ls = new Set<() => void>();
  const init = () => { if (!sub) { sub = true; onPrivateReadsPaused(() => { pending = null; }); } };
  return {
    set(v: T) { init(); pending = v; for (const fn of ls) fn(); },
    take(m?: (v: T) => boolean): T | null { init(); if (pending === null || (m && !m(pending))) return null; const out = pending; pending = null; return out; },
    sub(fn: () => void) { init(); ls.add(fn); return () => { ls.delete(fn); }; },
  };
}

const prefill = makeSignal<CanvasTemplatePrefill>(), saveReq = makeSignal<string>();
const reviewReq = makeSignal<{ target: HostedTemplateReviewTarget; opener?: HTMLElement }>();
export const stageCanvasTemplatePrefill = (p: CanvasTemplatePrefill) => prefill.set(p);
export const consumeCanvasTemplatePrefill = () => prefill.take();
export const onCanvasTemplatePrefill = (fn: () => void) => prefill.sub(fn);
export const requestSaveAsTemplateForArtifact = (id: string) => saveReq.set(id);
export const consumeSaveAsTemplateRequest = (id: string) => saveReq.take((v) => v === id) !== null;
export const onSaveAsTemplateRequested = (fn: () => void) => saveReq.sub(fn);
export const requestCanvasTemplateReview = (target: HostedTemplateReviewTarget, opener?: HTMLElement) => reviewReq.set({ target, opener });
export const consumeCanvasTemplateReview = () => reviewReq.take();
export const onCanvasTemplateReview = (fn: () => void) => reviewReq.sub(fn);
