/** Exercise the Agents screen, scoped views and detail read failures with real API shapes. */
declare function require(name: string): any;
const { Window } = require("happy-dom");
const win = new Window({ url: "http://localhost/agents" });
for (const name of [
  "window",
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLTextAreaElement",
  "HTMLSelectElement",
  "SVGElement",
  "navigator",
  "MutationObserver",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "location",
  "history",
]) {
  Object.defineProperty(globalThis, name, {
    value: name === "window" ? win : win[name],
    configurable: true,
    writable: true,
  });
}
for (const name of ["addEventListener", "removeEventListener"])
  Object.defineProperty(globalThis, name, {
    value: win[name].bind(win),
    configurable: true,
  });
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
const loader = require("module");
const originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons")
    return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const React = require("react") as typeof import("react");
const { createRoot } =
  require("react-dom/client") as typeof import("react-dom/client");
const Agents = (
  require("../src/features/agents/Agents") as typeof import("../src/features/agents/Agents")
).default;
const { LocaleProvider } = require("../src/lib/locale") as typeof import("../src/lib/locale");
const { navigate } = require("../src/lib/useLocation") as typeof import("../src/lib/useLocation");
const { locationHref, readLocation } = require("../src/lib/router") as typeof import("../src/lib/router");
const { routineDraftReviewHref, routineReviewHref } = require("../src/features/home/needs") as typeof import("../src/features/home/needs");
const { allowPrivateReads, resetPrivateSession } = require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
const { NavigationProvider } = require("../screen-ui/src/navigation") as typeof import("../screen-ui/src/navigation");
const { hostNavigation } = require("../src/ui/hostNavigation") as typeof import("../src/ui/hostNavigation");
type Agent = import("../src/lib/types").Agent;
type AgentsPayload = import("../src/lib/types").AgentsPayload;
const base: Agent = {
  alias: "dev",
  provider: "pi",
  endpoint_kind: "pty",
  role: "Developer",
  state: "idle",
  group: "pm",
  group_root: false,
  tasks: [],
  running: 0,
  queued: 0,
  unknown: 0,
  parked: 0,
  fenced: false,
  on: [],
};
const fresh = { by_issue: {}, refresh_error: false, snapshot_age_secs: 0, stale: false, daemon: "reachable", totals: null } as const;
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const personalRow = { scope: "personal", opens: "home", name: "Nova", look: "robot", bootstrap_state: "complete", conversation_id: "c1" } as const;
const crm: Agent = { ...base, alias: "crm-action-review", running: 1, state: "busy" };
const roster = (over: Partial<AgentsPayload> & { master?: Partial<Agent> } = {}): AgentsPayload => {
  const { master, ...rest } = over;
  return { ...fresh, profiles: [], agents: [{ ...base, alias: "master", ...master }, crm], ...rest } as AgentsPayload;
};
const hostedRoster = (extra: Partial<AgentsPayload> = {}) =>
  roster({ master: { scope: "workspace", name: "Acme" }, personal_assistant: personalRow, ...extra });
let memberStateReads = 0;
let aliasReads = 0;
let masterReads = 0;
let connectionsReads = 0;
let threadReads: string[] = [];
let directoryReads = 0;
let directoryAvailable = true;
const routineListAliases: (string | null)[] = [];
const endpointQueryAgents: (string | null)[] = [];
const personalAssistantId = "assistant_0123456789abcdef0123456789abcdef";
let personalSettingsMode = false;
let personalRev = 1;
let personalName: string | null = "Nova";
let personalTone: string | null = null;
let personalLook = "robot";
let personalInstructions: string | null = "Personal prompt";
let personalSkills: string[] = [];
let personalEnabledTools: string[] = [];
let lastPersonalPatch: Record<string, unknown> | null = null;
let memoryFiles: Record<string, { text: string; sha: string }> = {
  "SOUL.md": {
    text: "# Soul\n\n<!-- cadence:managed-begin tone -->\nTone: Concise\n<!-- cadence:managed-end tone -->\n\n<!-- cadence:managed-begin soul_body -->\nLead with conclusions.\n<!-- cadence:managed-end soul_body -->\n",
    sha: "sha_soul_1",
  },
  "USER.md": {
    text: "# User preferences\n\n<!-- cadence:managed-begin user_name -->\nUsername: Lee\n<!-- cadence:managed-end user_name -->\n",
    sha: "sha_user_1",
  },
  "MEMORY.md": {
    text: "# Memory\n\n- 2026-11-26: Weekly summary goes out on Fridays.\n",
    sha: "sha_mem_1",
  },
  "memory/preferences.md": {
    text: "# preferences\n\n- Prefers tables over paragraphs.\n",
    sha: "sha_pref_1",
  },
};
let memoryHistory: Array<{
  id: string;
  at: string;
  op: string;
  target: string;
  actor: string;
  turn_id: string;
  before_exists: boolean;
  before_sha: string;
  after_sha: string;
  before_text: string;
  added: string[];
  removed: string[];
  undone_by: string | null;
}> = [];
let memorySeq = 1;
let lastMemoryEdit: Record<string, unknown> | null = null;
let lastMemoryUndo: string | null = null;
globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "http://localhost");
  const path = url.pathname;
  if (path === "/api/team/members") {
    directoryReads += 1;
    if (!directoryAvailable) return json({ error: "unavailable" }, 503);
    return json({ members: [
      { userId: "viewer", name: "Lee Chen", role: "member", joinedAt: "2026-11-16" },
      { userId: "teammate", name: "Maya Park", email: "maya@example.test", role: "owner", joinedAt: "2026-11-16" },
      { userId: "unnamed", name: "hidden@example.test", email: "hidden@example.test", role: "member", joinedAt: "2026-11-16" },
    ] });
  }
  if (path === "/api/member-assistant/state") {
    memberStateReads += 1;
    return json({
      assistant: {
        assistant_id: personalAssistantId,
        alias: "assistant-1",
        identity: { name: personalName, tone: personalTone, look: personalLook, revision: personalRev },
      },
      conversation_id: "c1",
    });
  }
  if (path === "/api/member-assistant/instructions") {
    if (!personalSettingsMode) return json({ error: "not found" }, 404);
    return json({
      instructions: personalInstructions,
      revision: personalRev,
      built_in_instruction_tokens: 120,
      skills: personalSkills,
      enabled_tools: personalEnabledTools,
    });
  }
  if (path === "/api/member-assistant/profile" && personalSettingsMode) {
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    lastPersonalPatch = body;
    if (body.expected_revision !== personalRev) return json({ code: "revision_conflict", message: "stale revision" }, 409);
    if (Array.isArray(body.enabled_tools) && operator !== true) {
      const restricted = body.enabled_tools.filter(
        (g): g is string => typeof g === "string" && g !== "cadence_writes" && g !== "whatsapp_read",
      );
      if (restricted.length > 0) {
        return json({ code: "owner_required", message: "Only workspace owners may change workspace opt-in tool groups." }, 403);
      }
    }
    if (typeof body.name === "string") personalName = body.name.trim() || null;
    if (typeof body.tone === "string") personalTone = body.tone.trim() || null;
    if (typeof body.look === "string") personalLook = body.look;
    if (typeof body.instructions === "string") personalInstructions = body.instructions.trim() || null;
    if (Array.isArray(body.skills)) personalSkills = body.skills.filter((s): s is string => typeof s === "string");
    if (Array.isArray(body.enabled_tools)) {
      personalEnabledTools = body.enabled_tools.filter((g): g is string => typeof g === "string");
    }
    personalRev += 1;
    return json({
      assistant: {
        assistant_id: personalAssistantId,
        alias: "assistant-1",
        identity: { name: personalName, tone: personalTone, look: personalLook, revision: personalRev },
      },
      conversation_id: "c1",
      action: "update",
      skills: personalSkills,
      enabled_tools: personalEnabledTools,
      applies: "next_turn",
    });
  }
  if (path === "/api/member-assistant/memory-get" && personalSettingsMode) {
    return json({
      files: ["SOUL.md", "USER.md", "MEMORY.md"].map((target) => {
        const entry = memoryFiles[target] ?? { text: "", sha: "empty" };
        return {
          target,
          text: entry.text,
          sha: entry.sha,
          line_count: entry.text.trimEnd() === "" ? 0 : entry.text.trimEnd().split("\n").length,
          ...(target === "MEMORY.md" ? { cap: 200 } : {}),
        };
      }),
      topics: Object.entries(memoryFiles)
        .filter(([k]) => k.startsWith("memory/"))
        .map(([target, entry]) => ({
          slug: target.slice("memory/".length, -".md".length),
          target,
          text: entry.text,
          sha: entry.sha,
          line_count: entry.text.trimEnd() === "" ? 0 : entry.text.trimEnd().split("\n").length,
        })),
      history: memoryHistory.map(({ before_text: _bt, ...rest }) => rest),
      applies: "next_turn",
    });
  }
  if (path === "/api/member-assistant/memory-edit" && personalSettingsMode) {
    const body = JSON.parse(String(init?.body ?? "{}")) as {
      target: string;
      content: string;
      expected_sha?: string;
    };
    lastMemoryEdit = body;
    const prev = memoryFiles[body.target];
    if (body.expected_sha !== undefined && prev && body.expected_sha !== prev.sha) {
      return json({ ok: false, error: { code: "revision_conflict", message: `${body.target} changed since it was loaded` } }, 409);
    }
    memorySeq += 1;
    const nextSha = `sha_${memorySeq}`;
    const histId = `mem_${memorySeq}`;
    memoryHistory.unshift({
      id: histId,
      at: "2026-11-26T09:00:00Z",
      op: "member_edit",
      target: body.target,
      actor: "users/viewer",
      turn_id: "member-ui",
      before_exists: prev !== undefined,
      before_sha: prev?.sha ?? "",
      after_sha: nextSha,
      before_text: prev?.text ?? "",
      added: [body.content.trim().split("\n").at(-1) ?? ""],
      removed: [],
      undone_by: null,
    });
    memoryFiles[body.target] = { text: body.content, sha: nextSha };
    return json({ target: body.target, sha: nextSha, history_id: histId, applies: "next_turn" });
  }
  if (path === "/api/member-assistant/memory-undo" && personalSettingsMode) {
    const body = JSON.parse(String(init?.body ?? "{}")) as { history_id: string };
    lastMemoryUndo = body.history_id;
    const idx = memoryHistory.findIndex((h) => h.id === body.history_id);
    if (idx < 0 || memoryHistory[idx]!.undone_by) {
      return json({ ok: false, error: { code: "revision_conflict", message: "cannot undo" } }, 409);
    }
    const item = memoryHistory[idx]!;
    memorySeq += 1;
    const undoId = `mem_${memorySeq}`;
    item.undone_by = undoId;
    if (item.before_exists) {
      memoryFiles[item.target] = { text: item.before_text, sha: item.before_sha };
    } else {
      delete memoryFiles[item.target];
    }
    return json({
      undone: item.id,
      undo_history_id: undoId,
      op: item.op,
      target: item.target,
      sha: item.before_sha,
      revision_sha: item.before_sha,
      applies: "next_turn",
    });
  }
  // CAD-2354: an API that predates the instructions routes.
  if (path === "/api/master/instructions") return json({ error: "not found" }, 404);
  if (path === "/api/workspace-skills" && personalSettingsMode) {
    return json({
      skills: [
        {
          name: "weekly-report",
          description: "Summarize the week",
          approved: true,
          latest_revision: 1,
          latest_digest: "abc",
          approved_revision: 1,
          approved_digest: "abc",
          used_by: [],
        },
        {
          name: "draft-skill",
          description: "Unapproved draft",
          approved: false,
          latest_revision: 1,
          latest_digest: "def",
          approved_revision: null,
          approved_digest: null,
          used_by: [],
        },
      ],
    });
  }
  if (path === "/api/endpoints" && personalSettingsMode) {
    endpointQueryAgents.push(url.searchParams.get("agent"));
    return json({
      endpoints: [
        {
          id: "ep_123",
          agent_uid: personalAssistantId,
          kind: "email",
          slug: "nova",
          state: "active",
          verified: true,
          config_revision: 1,
          deleted_at: null,
          sender_allowlist: [],
          target_kind: "isolated_inbox",
          routine_id: null,
          binding_workflow: "",
          binding_digest: "",
          owner_subject_id: "viewer",
          auth_mode: null,
          secret_hint: null,
          secret_rotated_at: null,
          prev_secret_expires_at: null,
          rate_limit_rpm: 60,
          webhook_url: null,
          hold_capacity: 50,
          created_at: 1,
          updated_at: 1,
          address: "nova.acme@cadencecloud.app",
          enabled: true,
          event_source: "email.inbound",
        },
      ],
    });
  }
  if (path === "/api/connections" && personalSettingsMode) {
    connectionsReads += 1;
    return json({ connections: [] });
  }
  if (path.startsWith("/api/threads/")) {
    threadReads.push(path);
    return json({ entries: [], cursor: 0, more_before: false });
  }
  if (path === "/api/routines") {
    routineListAliases.push(url.searchParams.get("agent"));
    if (personalSettingsMode && url.searchParams.get("agent") === personalAssistantId) {
      return json({
        routines: [
          {
            id: "rt_personal_1",
            name: "Inbox digest",
            trigger_kind: "event",
            event_source: "email.inbound",
            agent_alias: personalAssistantId,
            target_kind: "markdown",
            markdown_path: "wiki:global/playbooks/digest.md",
            status: "on",
            thread_id: "c1",
          },
        ],
        staged_proposals: [],
        waiting_runs: [],
      });
    }
    return json({ routines: [], staged_proposals: [
      { id: "omitted", state: "pending", title: "Alias omitted", kind: "routine_propose", payload: {}, proposed_by: "assistant" },
      { id: "padded", state: "pending", title: "Alias padded", kind: "routine_propose", payload: { agent_alias: " master " }, proposed_by: "assistant" },
      { id: "null", state: "pending", title: "Alias null", kind: "routine_propose", payload: { agent_alias: null }, proposed_by: "assistant" },
      { id: "saved-assistant", state: "pending", title: "Saved assistant routine draft", kind: "routine_propose", payload: { agent_alias: "assistant" }, proposed_by: "assistant" },
      { id: "other", state: "pending", title: "Another runtime’s routine", kind: "routine_propose", payload: { agent_alias: "worker" }, proposed_by: "master" },
    ] });
  }
  if (path.startsWith("/api/agents/")) {
    const alias = decodeURIComponent(path.split("/").pop()!);
    if (alias === "assistant") aliasReads += 1;
    if (alias === "master") masterReads += 1;
    return json({ agent: { alias, provider: "pi", endpoint_kind: "pty", instructions: "x" }, queued: 0, unknown: 0, running: [], events: [], fenced: false, on: [] });
  }
  return json({ error: "unavailable" }, 503);
};
function assert(value: unknown, what: string): asserts value {
  if (!value) throw new Error(what);
}
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const loaded = <T>(data: T): import("../src/lib/cache").ResourceState<T> => ({
  data, status: "ok", error: null, asOf: Date.now(), inFlight: false,
});
const flush = async () => {
  await React.act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
};
let payload: AgentsPayload = roster();
let hostedBoard = true;
let operator: boolean | null = true;
let viewerSub: string | null = "viewer";
let profiles: unknown[] = [];
let rosterFailed = false;
let rosterLoading = false;
/** Reads the route from the real URL, as the app does. */
function route() {
  const r = readLocation(location.pathname, location.search).route;
  return r.screen === "agents" ? r : { alias: null, profileId: undefined, tab: undefined, routineAgent: undefined };
}
async function render() {
  const { alias: open, profileId: openProfileId, tab = "overview", routineAgent } = route();
  const viewer = { viewerSub: hostedBoard ? viewerSub : null };
  await React.act(() =>
    root.render(
      React.createElement(NavigationProvider, { adapter: hostNavigation, children:
      React.createElement(LocaleProvider, null, React.createElement(Agents, {
        key: `${operator}${hostedBoard}`,
        operator,
        onRetryAccess: () => {},
        hostedBoard,
        ...viewer,
        openProfileId,
        state: rosterFailed
          ? { ...loaded<AgentsPayload | null>(null), status: "failed", error: "Offline" }
          : rosterLoading
            ? { ...loaded<AgentsPayload | null>(null), status: "loading", inFlight: true }
            : loaded({ ...payload, profiles } as AgentsPayload),
        issues: loaded([]),
        open,
        tab,
        routineAgent,
        recruit: false,
        ask: false,
        onAsk: () => {},
        // The app's own handler: a navigation, read back from the URL.
        onOpenAgent: (alias, to, profileId) => {
          const loc = readLocation(location.pathname, location.search);
          navigate(locationHref({ ...loc, route: { screen: "agents", alias, profileId, tab: to === "overview" ? undefined : to } }));
        },
        onTab: () => {},
        onToast: () => {},
        onRecruit: () => {},
        onOpenIssue: () => {},
        onRetry: () => {},
      })),
      }),
    ),
  );
  await flush();
}
async function at(path: string) {
  navigate(path);
  await render();
}
const seats = () => Array.from(host.querySelectorAll(".rc-seats .rc-seat")).map((b) => b.textContent ?? "");
const sideRows = () => Array.from(host.querySelectorAll(".ag-side-row")).map((b) => b.textContent ?? "");
async function click(el: Element | null | undefined) {
  assert(el, "the element to click exists");
  await React.act(() => (el as HTMLElement).click());
  await render();
}
async function main() {
  allowPrivateReads();
  const checkDirectory = () => {
    const cards = [...host.querySelectorAll(".rc-seats .rc-seat")];
    const maya = cards.find((card) => card.textContent?.includes("Maya Park"));
    assert(maya, "a teammate has a private assistant card");
    assert(maya.textContent?.includes("Personal assistant") && maya.textContent.includes("Private"), "the card describes privacy, not status");
    assert(!maya.matches("a, button, [tabindex], [role=button], [role=link]") && !maya.querySelector("a, button, [tabindex]"), "teammate card has no way to open it");
    assert(maya.querySelector(".agent-avatar") !== null, "teammate card wears an assistant avatar");
    assert(!sideRows().some((s) => s.includes("Maya Park")), "teammate is not a sidebar destination");
    assert(!host.textContent?.includes("Lee Chen") && !host.textContent?.includes("@example.test"), "viewer and email fallback are not rendered");
    assert(cards.some((card) => card.textContent?.includes("Teammate") && card.textContent.includes("Private")), "a missing public name still has a generic private card");
  };
  // A member on a hosted board: My assistant first, no Master row in the cards or the sidebar.
  payload = hostedRoster();
  operator = false;
  await at("/agents");
  assert(seats()[0].includes("Nova · Your assistant"), "member: My assistant is the first card");
  assert(!seats().some((s) => s.includes("master") || s.includes("Acme")), "member: no Master card");
  assert(sideRows()[1].includes("Nova · Your assistant"), "member: My assistant is the first sidebar row after Overview");
  assert(!sideRows().some((s) => s.includes("Acme") || s.includes("master")), "member: no Master sidebar row");
  assert(host.querySelector(".rc-seats .rc-seat svg, .rc-seats .rc-seat [class*=avatar]") !== null, "the card wears a face");
  checkDirectory();
  const search = host.querySelector<HTMLInputElement>("#agents-search");
  assert(search, "Agents offers search");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(search, "PARK   MAYA");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Maya Park"), "Agents search matches teammate names without an empty-state message");
  assert(!host.textContent?.includes("No agents match"), "teammate match is a roster result");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(search, "assistant   NOVA");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Nova · Your assistant"), "personal name and alias match in either order");

  // An owner: My assistant first, then Master labelled company and owners-only (workspace name as the title).
  operator = true;
  await render();
  assert(seats()[0].includes("Nova · Your assistant"), "owner: My assistant first");
  assert(seats()[1].includes("Acme") && seats()[1].includes("Master · company, owners only"), "owner: Master follows, labelled");
  assert(sideRows().some((s) => s.includes("Master · company, owners only")), "owner: the sidebar row is labelled too");
  checkDirectory();
  const ownerSearch = host.querySelector<HTMLInputElement>("#agents-search");
  assert(ownerSearch, "owner: Agents offers search");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(ownerSearch, "ACME master");
    ownerSearch.dispatchEvent(new Event("input", { bubbles: true }));
  });
  assert(seats().length === 1 && seats()[0].includes("Acme"), "owner: search matches the displayed Master name and alias");
  assert(sideRows().slice(1).length === 1 && sideRows()[1].includes("Acme"), "owner: search finds Master in the sidebar by display name too");
  await React.act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(ownerSearch, "");
    ownerSearch.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const priorReads = directoryReads;
  viewerSub = null;
  await render();
  assert(!host.textContent?.includes("Maya Park") && directoryReads === priorReads, "an unidentified viewer does not read or render the directory");
  directoryAvailable = false;
  viewerSub = "viewer";
  await render();
  assert(!host.textContent?.includes("Maya Park"), "a failed read does not display earlier directory data");
  directoryAvailable = true;

  // Clicking Master (card, then sidebar row) navigates to the Master page, with no duplicate chat.
  await at("/agents");
  await click(host.querySelector(".rc-seats [aria-label='Open agent master']"));
  assert(location.pathname === "/agents/master" && location.search === "?tab=instructions", "the Master card navigates to its page");
  assert(host.querySelector(".ap-hero h1")?.textContent === "master", "the Master page, not a chat, opens");
  assert(host.querySelector(".ag-chat") === null, "no chat opens for the company Master");
  await at("/agents");
  const row = Array.from(host.querySelectorAll<HTMLElement>(".ag-side-row")).find((b) => b.textContent?.includes("Master · company"));
  await click(row);
  assert(location.pathname === "/agents/master", "the Master sidebar row navigates");
  // Live worker's card opens its consolidated Overview (zero duplicate chat) and links to Channels.
  await at("/agents");
  threadReads = [];
  await click(host.querySelector(".rc-seats [aria-label='Open agent crm-action-review']"));
  assert(
    String(location.pathname) === "/agents/crm-action-review" &&
      host.querySelector("main.agents-page") !== null &&
      host.querySelector(".ag-chat") === null &&
      host.querySelector("[data-composer]") === null &&
      threadReads.length === 0 &&
      Array.from(host.querySelectorAll("a")).some(
        (a) => a.textContent?.includes("Mention in channel") && a.getAttribute("href") === "/threads",
      ),
    "live worker opens consolidated Overview with Mention in channel CTA and zero duplicate chat",
  );

  // A member who types /agents/master reads the owners-only gate; the assistant is on Home.
  operator = false;
  await at("/agents/master");
  assert(
    host.textContent?.includes("Chat with the company Master is for owners. Your assistant is on Home."),
    "member: the gate says who Master is for",
  );
  assert(host.querySelector("textarea") === null, "member: no composer for the company Master");
  threadReads = [];
  await at("/agents/crm-action-review");
  assert(
    host.querySelector("main.agents-page") !== null &&
      host.querySelector(".ag-chat") === null &&
      host.querySelector("[data-composer]") === null &&
      threadReads.length === 0,
    "member: consolidated worker page renders Overview without a duplicate chat composer or thread reads",
  );
  assert(!host.textContent?.includes("Chat with the company Master is for owners."), "the Master refusal stays private to the Master route");

  const priorMasterReads = masterReads;
  rosterLoading = true;
  await at("/agents/master?tab=instructions");
  assert(masterReads === priorMasterReads && host.querySelector(".ap-hero") === null, "loading hosted roster never mounts or reads Master");
  rosterLoading = false;
  rosterFailed = true;
  await render();
  assert(masterReads === priorMasterReads && host.textContent?.includes("Could not load agents"), "failed hosted roster never reads Master and offers retry");
  rosterFailed = false;

  // An initial roster failure is not proof that a hosted alias is a hired agent.
  rosterFailed = true;
  await at("/agents/assistant?tab=instructions");
  assert(aliasReads === 0, "failed hosted roster never reads the reserved alias");
  assert(host.textContent?.includes("Could not load agents") && host.querySelector("main [role=alert] button"), "a failed roster offers retry");
  rosterFailed = false;

  // /agents/assistant is the personal page, even beside a hired profile named assistant.
  profiles = [{ id: "ap_1", alias: "assistant", role: "worker", team_role: "worker", catalogue: null, look: null, job: "x", model: "inherit", runtimes: [] }];
  operator = true;
  await at("/agents/assistant");
  assert(host.querySelector(".ap-hero h1")?.textContent === "My assistant", "the personal page wins over a hired `assistant`");
  assert(host.textContent?.includes("Nova · Your assistant"), "the header names the assistant");
  assert(memberStateReads > 0, "the header reads /api/member-assistant/state");
  assert(host.textContent?.includes("not edited here yet"), "Instructions is the honest read-only note");
  assert(!host.textContent?.includes("Manage profile"), "no fake controls");
  assert(host.querySelectorAll("[aria-current='page']").length === 1, "one sidebar row is current");
  const homeLink = [...host.querySelectorAll("a")].find((link) => link.textContent === "Open Home");
  assert(homeLink?.getAttribute("href") === "/?chat=assistant", "Open Home offers the owner's own assistant chat URL");
  assert(host.querySelector("[aria-current='page']")?.textContent?.includes("Your assistant"), "the personal row is the current one");
  await click(homeLink);
  assert(String(location.pathname) === "/" && String(location.search) === "?chat=assistant", "Open Home navigates to the owner's own assistant chat");
  await at("/agents");
  await click(host.querySelector(".rc-seats [aria-label='Open agent assistant']"));
  assert(String(location.pathname) === "/agents/profile/ap_1", "a hired assistant opens its own ID route instead of the caller's assistant");
  assert(host.querySelector("h1")?.textContent === "assistant" && aliasReads === 0, "the profile stays separate without an agent alias read");
  assert(host.querySelectorAll("[aria-current='page']").length === 1 && host.querySelector("[aria-current='page']")?.textContent?.startsWith("assistant"), "only the hired profile row is current");
  await at("/agents/profile/ap_1?tab=instructions");
  assert(host.querySelector("[role=tab][aria-selected=true]")?.textContent === "Instructions" && aliasReads === 0, "a hired profile ID deep link keeps the requested tab");
  const routineHref = routineReviewHref("assistant", { ...payload, profiles } as AgentsPayload);
  assert(routineHref === "/agents/profile/ap_1?tab=routines", "Home routine review opens the hired assistant by ID, not the private assistant");
  await at(routineHref);
  assert(host.querySelector("[role=tab][aria-selected=true]")?.textContent === "Routines" && aliasReads === 0, "hired routine review opens the Routines tab without an alias detail read");
  const draft = { kind: "routine_propose", payload: {}, proposed_by: "assistant" };
  assert(routineDraftReviewHref(draft, { ...payload, profiles } as AgentsPayload) === routineHref, "a hosted draft without an explicit alias reviews its effective hired target");
  assert(routineDraftReviewHref({ ...draft, proposed_by: "master" }, hostedRoster()) === "/agents/master?tab=routines", "the usual hosted draft without an alias still opens Master Routines for review");
  assert(routineDraftReviewHref({ ...draft, payload: { agent_alias: "master" } }, hostedRoster()) === "/agents/master?tab=routines", "the explicit execution target takes precedence over the saved fallback");
  assert(routineReviewHref(null, roster()) === "/agents/master?tab=routines", "a legacy hosted proposal without an alias keeps its review destination");
  assert(routineReviewHref(null, hostedRoster()) === null, "scoped Home does not guess a missing routine target");
  const unresolvedDraft = routineDraftReviewHref(draft, hostedRoster());
  assert(unresolvedDraft === "/agents/master?tab=routines", "an alias-less draft without a hired target still opens review");
  assert(routineDraftReviewHref({ ...draft, payload: { agent_alias: " master " } }, hostedRoster()) === unresolvedDraft, "draft navigation normalizes the explicit alias");
  assert(routineDraftReviewHref({ ...draft, payload: { agent_alias: 42 } }, hostedRoster()) === null, "a malformed explicit alias does not get the omitted-alias fallback");
  await at(unresolvedDraft);
  const routineTab = host.querySelector("[data-routines-tab]")?.textContent;
  assert(routineTab?.includes("Alias omitted") && routineTab.includes("Alias padded") && routineTab.includes("Alias null") && !routineTab.includes("Another runtime’s routine"), "the destination displays the routed drafts and does not mix in other explicit aliases");
  await at("/agents");
  await click([...host.querySelectorAll(".ag-side-row")].find((row) => row.textContent?.startsWith("assistant")));
  assert(String(location.pathname) === "/agents/profile/ap_1", "the hired sidebar row opens the same ID route");
  await at("/agents/profile/ap_missing");
  assert(host.textContent?.includes("Agent details are unavailable.") && aliasReads === 0, "an unknown profile ID never falls back to the personal route");
  await at("/agents/assistant");
  // Without a personal row (an older API) the same route is the hired profile, as today.
  payload = roster();
  await render();
  assert(host.querySelector("h1")?.textContent === "assistant", "an older API: /agents/assistant is the hired profile");
  profiles = [];
  payload = hostedRoster();
  const savedRoutine = routineReviewHref("assistant", payload);
  assert(savedRoutine === "/agents/master?tab=routines&routineAgent=assistant", "a stored routine still has a Review destination when the profile is missing");
  const privateReadsBeforeRoutine = memberStateReads;
  await at(savedRoutine);
  assert(routineListAliases.at(-1) === "assistant" && host.querySelector("[data-routines-tab]")?.textContent?.includes("Saved assistant routine draft"), "Review opens the list for the stored alias rather than Master’s list");
  assert(memberStateReads === privateReadsBeforeRoutine, "workspace routine review does not open the caller’s private assistant");
  operator = false;
  const approvalsReads = routineListAliases.length;
  await at(savedRoutine);
  assert(routineListAliases.length === approvalsReads && host.textContent?.includes("Chat with the company Master is for owners."), "the stored-alias Review route never gives members access or reads routines");

  // An older API (no scope, no personal_assistant) renders as today, Master shown to everyone.
  payload = roster();
  const legacyReads = directoryReads;
  for (const who of [false, true]) {
    operator = who;
    await at("/agents");
    assert(seats().length === 2 && seats()[0].startsWith("master"), "old roster: Master leads, as today");
    assert(!seats().some((s) => s.includes("Your assistant")), "old roster: no personal card");
    assert(!seats()[0].includes("owners only"), "old roster: Master keeps its old label");
    assert(!sideRows().some((s) => s.includes("Your assistant") || s.includes("owners only")), "old roster: sidebar as today");
  }
  // A native board never hides or relabels Master.
  hostedBoard = false;
  operator = false;
  await at("/agents");
  assert(seats()[0].startsWith("master"), "native: Master is unchanged");
  assert(directoryReads === legacyReads, "legacy and native boards do not read the directory");

  // Aligned personal assistant tabs: Instructions | Routines | Skills | Tools + owner/member gating.
  personalSettingsMode = true;
  hostedBoard = true;
  operator = true;
  payload = hostedRoster();
  await at("/agents/assistant");
  const personalTabLabels = Array.from(host.querySelectorAll(".ap-tabs [role=tab]")).map((el) => el.textContent);
  assert(
    JSON.stringify(personalTabLabels) === JSON.stringify(["Instructions", "Memory", "Routines", "Skills", "Tools"]),
    "My assistant renders Instructions, Memory, Routines, Skills, and Tools tabs",
  );
  assert(
    host.querySelector("[role=tab][aria-selected=true]")?.textContent === "Instructions",
    "default route on /agents/assistant lands on Instructions",
  );
  await at("/agents/assistant?tab=routines");
  assert(routineListAliases.at(-1) === personalAssistantId, "Routines tab queries by personal assistant_id");
  assert(endpointQueryAgents.at(-1) === personalAssistantId, "Incoming endpoints queries by personal assistant_id");
  assert(
    host.textContent?.includes("Inbox digest") && host.textContent?.includes("nova.company@cadencecloud.app"),
    "owner sees personal routines and incoming email endpoint",
  );
  const readPatch = () => lastPersonalPatch;
  await at("/agents/assistant?tab=skills");
  assert(host.textContent?.includes("weekly-report") && host.textContent?.includes("New skill"), "owner sees workspace skills and New skill action");
  const skillSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='weekly-report']");
  await click(skillSwitch);
  assert(
    readPatch()?.expected_revision === 1 && JSON.stringify(readPatch()?.skills) === JSON.stringify(["weekly-report"]),
    "toggling a skill sends expected_revision and skills to /api/member-assistant/profile",
  );
  await at("/agents/assistant?tab=tools");
  const toolSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='Cadence writes']");
  assert(toolSwitch && !toolSwitch.disabled, "owner can toggle opt-in tool groups");
  await click(toolSwitch);
  assert(
    readPatch()?.expected_revision === 2 && JSON.stringify(readPatch()?.enabled_tools) === JSON.stringify(["cadence_writes"]),
    "toggling an opt-in tool sends expected_revision and enabled_tools to /api/member-assistant/profile",
  );

  // Non-owner member: owns their personal Routines, Incoming endpoints, Skills, and personal-safe Tools; owner-only tools stay locked.
  operator = false;
  personalEnabledTools = [];
  const connectionsBeforeMember = connectionsReads;
  await at("/agents/assistant?tab=routines");
  assert(host.textContent?.includes("Private to you"), "member sees Private to you indicator in personal assistant header");
  assert(routineListAliases.at(-1) === personalAssistantId, "member Routines tab queries by their personal assistant_id");
  assert(endpointQueryAgents.at(-1) === personalAssistantId, "member Incoming endpoints queries by their personal assistant_id");
  assert(connectionsReads === connectionsBeforeMember, "member Routines tab never calls owner-only /api/connections");
  assert(
    host.textContent?.includes("Inbox digest") &&
      host.textContent?.includes("nova.company@cadencecloud.app") &&
      host.textContent?.includes("private hosted assistant session"),
    "member sees personal routines, incoming email endpoint, and hosted session/privacy callout",
  );
  const newRoutineBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent?.includes("+ New routine"));
  assert(newRoutineBtn !== undefined, "member sees + New routine button for personal assistant");
  await at("/agents/assistant?tab=tools");
  const memberSafeSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='Cadence writes']");
  const memberWhatsappReadSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='WhatsApp read']");
  const memberLockedSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='WhatsApp lifecycle']");
  assert(memberSafeSwitch && !memberSafeSwitch.disabled, "member can toggle personal-safe Cadence writes tool");
  assert(memberWhatsappReadSwitch && !memberWhatsappReadSwitch.disabled, "member can toggle personal-safe WhatsApp read tool");
  assert(memberLockedSwitch === null, "member sees workspace-owner-only tool groups read-only without a toggle switch");
  assert(host.textContent?.includes("Workspace owner only"), "member sees Workspace owner only indicator on locked tools");
  assert(
    host.textContent?.includes("You can enable personal-safe tools here."),
    "member sees updated opt-in tools note on Tools tab",
  );
  await click(memberWhatsappReadSwitch);
  assert(
    readPatch()?.expected_revision === 3 && JSON.stringify(readPatch()?.enabled_tools) === JSON.stringify(["whatsapp_read"]),
    "member toggling WhatsApp read saves enabled_tools to /api/member-assistant/profile",
  );
  await at("/agents/assistant?tab=skills");
  assert(!host.textContent?.includes("New skill"), "member does not see library management button on Skills tab");
  const memberSkillSwitch = host.querySelector<HTMLButtonElement>("button[role=switch][aria-label*='weekly-report']");
  await click(memberSkillSwitch);
  assert(
    readPatch()?.expected_revision === 4 && JSON.stringify(readPatch()?.skills) === JSON.stringify([]),
    "member can toggle their own assistant skills",
  );

  // View-first Hero identity editing: Avatar popover + inline Edit + shared revision sync with Instructions.
  await at("/agents/assistant?tab=instructions");
  const avatarBtn = host.querySelector<HTMLButtonElement>(".ap-avatar-btn");
  assert(avatarBtn !== null, "hero avatar button allows changing avatar");
  await click(avatarBtn);
  const avatarPopover = host.querySelector<HTMLElement>("[data-avatar-popover]");
  assert(
    avatarPopover !== null && avatarPopover.querySelectorAll("[data-assistant-look]").length === 11,
    "avatar popover offers all 11 looks",
  );
  await click(avatarPopover.querySelector<HTMLButtonElement>('[data-assistant-look="designer"]'));
  assert(
    readPatch()?.expected_revision === 5 && readPatch()?.look === "designer",
    "clicking an avatar in the popover saves look immediately with expected_revision",
  );
  assert(host.querySelector("[data-avatar-popover]") === null, "avatar popover closes after choosing look");

  const editBtn = host.querySelector<HTMLButtonElement>("[data-identity-edit]");
  assert(editBtn !== null, "quiet Edit button present in hero");
  await click(editBtn);
  const inlineForm = host.querySelector<HTMLFormElement>("[data-identity-inline-edit]");
  assert(inlineForm !== null, "clicking Edit opens inline identity editor");
  const [nameField, toneField] = Array.from(inlineForm.querySelectorAll<HTMLInputElement>("input.field"));
  assert(nameField && toneField, "inline identity editor renders Display name and Tone inputs");
  await React.act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!;
    desc.set!.call(nameField, "Atlas");
    nameField.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const presetChip = Array.from(inlineForm.querySelectorAll<HTMLButtonElement>(".ap-tone-chip")).find(
    (b) => b.textContent === "Concise & direct",
  );
  assert(presetChip !== undefined, "tone preset chip present");
  await click(presetChip);
  await click(inlineForm.querySelector<HTMLButtonElement>('button[type="submit"]'));
  assert(
    readPatch()?.expected_revision === 6 &&
      readPatch()?.name === "Atlas" &&
      readPatch()?.tone === "Concise & direct",
    "inline Save writes name and tone with latest revision",
  );
  assert(
    host.querySelector("[data-identity-inline-edit]") === null &&
      host.textContent?.includes("Atlas · Your assistant") &&
      host.textContent?.includes("Concise & direct"),
    "inline editor collapses and updates hero name and tone",
  );

  // Immediately editing Instructions after Identity uses the synced revision (7), avoiding a stale 409.
  const instArea = host.querySelector<HTMLTextAreaElement>("textarea.ap-doc-text");
  assert(instArea !== null, "Instructions textarea present below hero");
  await React.act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!;
    desc.set!.call(instArea, "Updated personal instructions");
    instArea.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const saveInstBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Save");
  assert(saveInstBtn !== undefined, "Instructions Save button appears when dirty");
  await click(saveInstBtn);
  assert(
    readPatch()?.expected_revision === 7 && readPatch()?.instructions === "Updated personal instructions",
    "Instructions save uses the revision synced from Identity save",
  );

  // Memory tab: inspect SOUL.md / MEMORY.md / topics, save edit with expected_sha, Undo, and 409 conflict recovery.
  await at("/agents/assistant?tab=memory");
  await new Promise((r) => setTimeout(r, 0));
  assert(
    host.textContent?.includes("SOUL.md") &&
      host.textContent?.includes("MEMORY.md") &&
      host.textContent?.includes("memory/preferences.md"),
    "Memory tab renders SOUL.md, MEMORY.md, and topic pills",
  );
  const memArea = host.querySelector<HTMLTextAreaElement>("textarea[aria-label='MEMORY.md']");
  assert(memArea !== null, "Memory tab renders textarea for MEMORY.md");
  await React.act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!;
    desc.set!.call(memArea, "# Memory\n\n- 2026-11-26: Weekly summary goes out on Fridays.\n- 2026-11-26: Updated fact.\n");
    memArea!.dispatchEvent(new Event("input", { bubbles: true }));
    memArea!.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const saveMemBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Save");
  assert(saveMemBtn && !saveMemBtn.disabled, "Save button activates when Memory draft is dirty");
  await click(saveMemBtn);
  assert(
    lastMemoryEdit?.target === "MEMORY.md" && lastMemoryEdit?.expected_sha === "sha_mem_1",
    "Save posts target, content, and expected_sha to /api/member-assistant/memory-edit",
  );
  assert(host.textContent?.includes("Updated fact."), "Memory history renders diff line after save");

  // External revision bump -> 409 conflict banner on next save.
  const staleSha = memoryFiles["MEMORY.md"]!.sha;
  memoryFiles["MEMORY.md"] = { text: "# Memory\n\n- External edit.\n", sha: "sha_external" };
  const memArea2 = host.querySelector<HTMLTextAreaElement>("textarea[aria-label='MEMORY.md']");
  await React.act(async () => {
    const desc = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!;
    desc.set!.call(memArea2, "# Memory\n\n- Stale client edit.\n");
    memArea2!.dispatchEvent(new Event("input", { bubbles: true }));
    memArea2!.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const saveMemBtn2 = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Save");
  await click(saveMemBtn2);
  assert(host.textContent?.includes("changed on disk"), "409 conflict displays reload banner");
  const reloadBtn = Array.from(host.querySelectorAll("button")).find((b) => b.textContent === "Reload latest");
  await click(reloadBtn);
  assert(
    host.querySelector<HTMLTextAreaElement>("textarea[aria-label='MEMORY.md']")?.value.includes("External edit.") &&
      staleSha !== memoryFiles["MEMORY.md"]!.sha,
    "Reload latest refreshes content and SHA after conflict",
  );

  const histUndoBtn = host.querySelector<HTMLButtonElement>(".ap-mem-hlist button.ap-link");
  assert(histUndoBtn !== null, "history list offers 1-click Undo button");
  await click(histUndoBtn);
  assert(lastMemoryUndo === memoryHistory[0]!.id, "history Undo posts history_id to memory-undo");

  assert(aliasReads === 0, "personal assistant tabs never fall back to /api/agents/assistant");

  await React.act(() => root.unmount());
  resetPrivateSession();
  console.log("my assistant checks passed");
}
main().catch(async (error) => {
  await React.act(() => root.unmount());
  resetPrivateSession();
  throw error;
});
export {};
