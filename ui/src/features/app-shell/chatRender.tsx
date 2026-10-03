import Md from "../../ui/Md";
import { stepSummary, toolSteps, type ThreadItem } from "../home/thread";

/**
 * CAD-1051 / CAD-1046: how the shell chat shows thread entries. Pure
 * helpers plus small views — nothing here reads a store or sends.
 */

export interface Directive {
  title: string;
  detail: string;
}

/** Message bodies that are machine directives (the CSV confirm handoff
 *  is `{"cadence_csv_import":{request_id,confirm_token}}`) render as a
 *  human card. Tokens, request ids and JSON never reach the screen. */
export function parseDirective(text: string): Directive | null {
  const body = text.trim();
  if (!body.startsWith("{")) return null;
  let value: unknown;
  try {
    value = JSON.parse(body);
  } catch {
    return null;
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const keys = Object.keys(value);
  if (keys.includes("cadence_csv_import")) {
    return { title: "Customer import", detail: "You confirmed the reviewed rows. The assistant applies them." };
  }
  if (/confirm_token|request_id/.test(body)) {
    return { title: "Confirmation sent", detail: "The assistant has your confirmation." };
  }
  return null;
}

/** Internal context ids (`ctx-…`) are plumbing, not copy. */
export function hideIds(text: string): string {
  return text.replace(/\bctx-[A-Za-z0-9_-]+/g, "this workspace");
}

export type ChatPage = "customers" | "segments" | "campaigns";

const PAGE_LABEL: Record<ChatPage, string> = {
  customers: "Customers",
  segments: "Segments",
  campaigns: "Campaigns",
};
const RECORD_LABEL: Record<ChatPage, string> = {
  customers: "Customer",
  segments: "Segment",
  campaigns: "Campaign",
};
const PAGE_PROMPTS: Record<ChatPage, string[]> = {
  customers: ["Who can't be emailed?", "Import a list", "Tag counter sign-ups"],
  segments: ["Build a segment for…", "Who can I email today?"],
  campaigns: ["Draft a welcome email", "Plan an October promo"],
};
const RECORD_PROMPTS: Record<ChatPage, string[]> = {
  customers: ["Summarise this customer", "Draft a follow-up", "Why can't they be emailed?"],
  segments: ["Refine this segment", "Explain who's excluded"],
  campaigns: ["Make it warmer", "Shorter subject", "Translate to Chinese"],
};

/** The chip names the page, or the open record, and offers 2–3 prompts
 *  that fill the composer (never send). */
export function chatContext(page: ChatPage, recordOpen: boolean): { label: string; prompts: string[] } {
  return recordOpen
    ? { label: `${RECORD_LABEL[page]} (open)`, prompts: RECORD_PROMPTS[page] }
    : { label: PAGE_LABEL[page], prompts: PAGE_PROMPTS[page] };
}

export function DirectiveCard({ directive }: { directive: Directive }) {
  return (
    <div className="card app-chat-card" data-chat-card="directive">
      <div className="text-label font-medium text-ink-100">{directive.title}</div>
      <div className="text-micro text-ink-400">{directive.detail}</div>
    </div>
  );
}

export function ChatRow({ item }: { item: ThreadItem }) {
  if (item.type === "operator" || item.type === "pending") {
    const text = item.type === "operator" ? item.entry.text : item.pending.text;
    const directive = parseDirective(text);
    if (directive) return <DirectiveCard directive={directive} />;
    return <div className="app-chat-me text-ink-100 whitespace-pre-wrap">{hideIds(text)}</div>;
  }
  if (item.type === "answer" || item.type === "commentary") {
    const directive = parseDirective(item.entry.text);
    if (directive) return <DirectiveCard directive={directive} />;
    return (
      <div className="app-chat-ai md text-ink-300" data-chat-md>
        <Md text={hideIds(item.entry.text)} />
      </div>
    );
  }
  if (item.type === "system") {
    return <p className="text-micro text-ink-500">· {hideIds(stepSummary(item.entry.text))}</p>;
  }
  const steps = toolSteps(item.entries);
  const open = steps.some((s) => !s.done);
  const failed = steps.some((s) => s.error);
  return (
    <p className="text-micro text-ink-500" data-chat-steps>
      {open ? "Working" : failed ? "Finished with an issue" : "✓ Done"} · {steps.length} step{steps.length === 1 ? "" : "s"}
    </p>
  );
}
