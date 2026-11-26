/**
 * CAD-2363: the pure text of an assistant turn — its elapsed time in the
 * reader's language, readable names for the hosted assistant's tools, the
 * subject a step acted on, and long hex identifiers as short tokens. No
 * React, so the UI tests load it directly.
 */

/** `8s`, `1m 5s`, `1h 3m` — templates the locale catalogue translates
 *  (written Chinese: `8 秒`, `1 分 5 秒`, `1 小時 3 分`). */
export function formatElapsed(secs: number, t: (english: string) => string = (e) => e): string {
  const s = Math.max(0, Math.floor(secs));
  const m = Math.floor(s / 60);
  const fill = (template: string, values: Record<string, number>) =>
    t(template).replace(/\{(\w)\}/g, (_, k: string) => String(values[k] ?? ""));
  if (s < 60) return fill("{s}s", { s });
  if (m < 60) return fill("{m}m {s}s", { m, s: s % 60 });
  return fill("{h}h {m}m", { h: Math.floor(m / 60), m: m % 60 });
}

/** Whole seconds between two instants (ms), or null when they do not measure. */
export function elapsedSecs(fromMs: number | null, toMs: number | null): number | null {
  if (fromMs === null || toMs === null || !Number.isFinite(fromMs) || !Number.isFinite(toMs) || toMs <= fromMs) return null;
  const secs = Math.round((toMs - fromMs) / 1000);
  return secs < 1 ? null : secs;
}

/**
 * The hosted assistant's tools (agenticos-v2 `tenant-board`) by what they do
 * for the reader. English keys; the locale catalogue translates them. A tool
 * not listed keeps its own name.
 */
const TOOL_LABELS: Record<string, string> = {
  account_usage: "Check account usage",
  activate_skill: "Load a skill",
  agent_message: "Message an agent",
  app_skill: "Load a skill",
  read_skill_resource: "Load a skill",
  app_call: "Use an app",
  app_describe: "Read app details",
  artifact_find: "Find artifacts",
  artifact_put: "Save an artifact",
  artifact_read: "Read an artifact",
  artifact_share: "Share an artifact",
  artifact_show: "Show on the canvas",
  ask_form: "Ask with a form",
  assistant_profile: "Read the assistant profile",
  board_create_issue: "Create a ticket",
  board_get_review_draft: "Read a review draft",
  board_list_issues: "List tickets",
  board_pending_actions: "Check pending approvals",
  board_read: "Read the board",
  board_read_issue: "Read a ticket",
  board_review_draft: "Draft a review",
  browser_control: "Use the browser",
  catalog_call: "Run a tool",
  catalog_describe: "Read tool details",
  catalog_result_read: "Read a tool result",
  catalog_search: "Search tools",
  connected_action: "Propose an account action",
  connected_read: "Read a connected account",
  chat_attachment_read: "Read an attachment",
  document_render: "Render a document",
  document_submit: "Submit for approval",
  document_type_propose: "Propose a document type",
  document_types: "Read document types",
  find_tools: "Find tools",
  job_status: "Check a job",
  mail_list: "List mail",
  mail_read: "Read mail",
  mcp_call_read: "Read from a connector",
  mcp_list_tools: "List connector tools",
  media_generate: "Generate media",
  propose_ticket: "Propose a ticket",
  ticket_attach: "Attach to a ticket",
  ticket_comment: "Comment on a ticket",
  ticket_review: "Send a ticket to review",
  tool_result_read: "Read a tool result",
  use_tool: "Use a tool",
  web_read: "Read a web page",
  web_search: "Search the web",
  wiki_list: "List wiki pages",
  wiki_read: "Read the wiki",
  wiki_search: "Search the wiki",
  workspace_skill: "Load a workspace skill",
  workspace_skill_draft: "Draft a workspace skill",
};

/** The tool name a step's summary starts with (`app_call {…}`, `web_search: q`). */
export function toolKey(summary: string): string {
  return summary.trim().split(/[\s:{(]/, 1)[0].toLowerCase();
}

/** The readable label (an English catalogue key) for a known tool, else null. */
export function toolLabel(summary: string): string | null {
  return TOOL_LABELS[toolKey(summary)] ?? null;
}

/** Argument fields that name what a call acted on, most specific first.
 *  Only these are ever shown: no other argument value reaches the row. */
const SUBJECT_KEYS = ["title", "name", "q", "query", "url", "path", "label", "id"];
const SUBJECT_MAX = 60;

/**
 * What a known tool call acted on, for the step row, read from the call's
 * full text (the stored summary is cut at 120 characters). A JSON argument
 * gives its first naming field (`{"q":"Gia"}` → `Gia`); `app_call` leads with
 * its operation (`contacts.search · Gia`); a plain argument is shown as is.
 * An argument that does not parse shows nothing rather than guessing.
 */
export function toolSubject(text: string): string {
  const firstLine = text.trim().split("\n", 1)[0];
  const key = toolKey(firstLine);
  const rest = text.trim().slice(key.length).replace(/^[\s:]+/, "").trim();
  if (rest === "") return "";
  let subject: string;
  if (rest.startsWith("{")) {
    let args: Record<string, unknown>;
    try {
      args = JSON.parse(rest) as Record<string, unknown>;
    } catch {
      return "";
    }
    if (!args || typeof args !== "object" || Array.isArray(args)) return "";
    if (key === "agent_message") {
      const strArg = (k: string) =>
        typeof args[k] === "string" ? (args[k] as string).trim() : "";
      const action = strArg("action");
      const parts =
        action === "read"
          ? [action, strArg("message")]
          : action === "inbox"
            ? [action, strArg("alias")]
            : [strArg("alias"), strArg("context_app")];
      subject = parts.filter((part) => part !== "").join(" · ");
    } else {
      const field = SUBJECT_KEYS.find((k) => typeof args[k] === "string" && (args[k] as string).trim() !== "");
      const named = field ? (args[field] as string) : "";
      const op = key === "app_call" && typeof args.op === "string" ? args.op : "";
      subject = [op, named].filter((part) => part !== "").join(" · ");
    }
  } else {
    subject = rest.split("\n", 1)[0];
  }
  subject = subject.replace(/\s+/g, " ").trim();
  return subject.length > SUBJECT_MAX ? `${subject.slice(0, SUBJECT_MAX - 1)}…` : subject;
}

/** Extract the primary diagnostic message from a `<harness>...</harness>` block,
 *  or null when `text` does not open with one. */
export function unwrapHarnessText(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("<harness>")) return null;
  const openEnd = "<harness>".length;
  const closeIdx = trimmed.indexOf("</harness>", openEnd);
  const inner = (closeIdx >= 0 ? trimmed.slice(openEnd, closeIdx) : trimmed.slice(openEnd)).trim();
  if (inner === "") return null;
  const lines = inner
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
  const tagged = (severity: string) =>
    lines.find((line) => new RegExp(`^\\[${severity}\\]`, "i").test(line));
  const chosen = tagged("error") ?? tagged("warn") ?? tagged("info") ?? lines[0];
  if (!chosen) return null;
  const msg = chosen.replace(/^\[(?:error|warn|info)\]\s*/i, "").trim();
  return msg === "" ? null : msg;
}

/** A failed tool's reason from a `<harness>` block and/or JSON result (`message`,
 *  else `error`), or null when the result has neither. */
export function errorReason(text: string): string | null {
  const unwrapped = unwrapHarnessText(text);
  const candidate = unwrapped ?? text;
  try {
    const value = JSON.parse(candidate) as Record<string, unknown> | null;
    if (value && typeof value === "object" && !Array.isArray(value)) {
      for (const k of ["message", "error"]) {
        if (typeof value[k] === "string" && (value[k] as string).trim() !== "") {
          return value[k] as string;
        }
      }
    }
  } catch {
    /* not JSON */
  }
  return unwrapped;
}

/** How many results a JSON tool result holds: an array's length, or the
 *  length of its `rows` / `items` / `results` / `data` list. Null otherwise. */
export function resultCount(text: string): number | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (Array.isArray(value)) return value.length;
  if (!value || typeof value !== "object") return null;
  for (const k of ["rows", "items", "results", "data"]) {
    const list = (value as Record<string, unknown>)[k];
    if (Array.isArray(list)) return list.length;
  }
  return null;
}

/**
 * A long hex identifier, optionally behind a short lowercase prefix
 * (`art-…`, `msg_…`): sixteen or more hex digits, at least one a–f, with no
 * letter, digit or hyphen run on either side. A pure number (an order or
 * card number) is never shortened.
 */
export const ID_TOKEN = /(?<![\w-])(?:[a-z]{2,8}[-_])?(?=[0-9]*[a-f])[0-9a-f]{16,}(?![\w-])/gi;

/** True when the whole string is one identifier. */
export function isIdToken(text: string): boolean {
  ID_TOKEN.lastIndex = 0;
  const m = ID_TOKEN.exec(text);
  ID_TOKEN.lastIndex = 0;
  return m !== null && m.index === 0 && m[0].length === text.length;
}

/** `art-5f3a90c1…c3c9`: the prefix, the first and last four hex digits. */
export function shortId(id: string): string {
  const m = id.match(/^((?:[a-z]{2,8}[-_])?)([0-9a-f]+)$/i);
  if (!m || m[2].length <= 12) return id;
  return `${m[1]}${m[2].slice(0, 4)}…${m[2].slice(-4)}`;
}

/** Plain text split into text and identifier parts, in order. */
export function splitIds(text: string): { id: boolean; text: string }[] {
  const out: { id: boolean; text: string }[] = [];
  let at = 0;
  for (const m of text.matchAll(ID_TOKEN)) {
    if (m.index > at) out.push({ id: false, text: text.slice(at, m.index) });
    out.push({ id: true, text: m[0] });
    at = m.index + m[0].length;
  }
  if (at < text.length) out.push({ id: false, text: text.slice(at) });
  return out;
}
