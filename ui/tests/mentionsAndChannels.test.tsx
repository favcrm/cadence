/** CAD-2711 frontend checks: shared @mention parser/picker, Channels UX, searchable Select, and Remove ConfirmDialog. */
declare function require<T = unknown>(name: string): T;
declare const process: { exitCode?: number };

import type * as ReactModule from "react";
import type { Root } from "react-dom/client";
import type * as MentionsModule from "../src/features/mentions/mentions";
import type * as CandidatesModule from "../src/features/mentions/candidates";
import type * as PickerModule from "../src/features/mentions/MentionPicker";
import type * as HumanThreadsModule from "../src/features/threads/HumanThreads";
import type * as NewConvModule from "../src/features/threads/NewHumanConversation";
import type * as NotificationToastModule from "../src/features/threads/NotificationToast";
import type * as HandoffModule from "../src/features/mentions/HomeMentionHandoff";
import type * as PartsModule from "../src/features/threads/ThreadChatParts";
import type * as ClientModule from "../src/features/threads/humanThreadsClient";
import type * as LocaleModule from "../src/lib/locale";
import type * as SessionModule from "../src/lib/privateSession";
import type * as SessionKeyModule from "../src/lib/sessionKey";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

async function main(): Promise<void> {
  const { Window } = require<{ Window: new (options: { url: string }) => Record<string, any> }>("happy-dom");
  const win = new Window({ url: "http://localhost/threads?h=room-1" });
  const pointerCtor = win.PointerEvent ?? win.MouseEvent;
  for (const name of [
    "window",
    "document",
    "Node",
    "Element",
    "HTMLElement",
    "HTMLButtonElement",
    "HTMLDialogElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "SVGElement",
    "navigator",
    "MutationObserver",
    "ResizeObserver",
    "Event",
    "MouseEvent",
    "KeyboardEvent",
    "location",
    "history",
    "sessionStorage",
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, name, {
      value: name === "window" ? win : win[name],
      configurable: true,
      writable: true,
    });
  }
  Object.defineProperty(globalThis, "PointerEvent", { value: pointerCtor, configurable: true, writable: true });
  for (const name of ["addEventListener", "removeEventListener"]) {
    Object.defineProperty(globalThis, name, { value: win[name].bind(win), configurable: true });
  }
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    value: (cb: (time: number) => void) => setTimeout(() => cb(Date.now()), 0),
    configurable: true,
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    value: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
    configurable: true,
  });
  Object.defineProperty(globalThis, "setInterval", { value: () => 1, configurable: true });
  Object.defineProperty(globalThis, "clearInterval", { value: () => undefined, configurable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true, configurable: true });
  win.HTMLElement.prototype.scrollIntoView = () => {};
  win.HTMLDialogElement.prototype.show = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.showModal = function (this: { open: boolean }) { this.open = true; };
  win.HTMLDialogElement.prototype.close = function (this: { open: boolean }) { this.open = false; };

  const loader = require<{ prototype: { require: (this: unknown, id: string) => unknown } }>("module");
  const originalRequire = loader.prototype.require;
  loader.prototype.require = function (this: unknown, id: string) {
    if (id.endsWith(".css")) return {};
    if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
    if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
    if (id === "react-markdown") return { __esModule: true, default: () => null };
    if (id === "remark-gfm") return { __esModule: true, default: () => undefined };
    return originalRequire.call(this, id);
  };

  const React = require<typeof ReactModule>("react");
  const { createRoot } = require<{ createRoot: (host: Element) => Root }>("react-dom/client");
  const {
    applyMention,
    filterMentionCandidates,
    findActiveMention,
    slugifyHandle,
    startsWithNote,
  } = require<typeof MentionsModule>("../src/features/mentions/mentions");
  const MentionPicker = require<typeof PickerModule>("../src/features/mentions/MentionPicker").default;
  const { isShareableAgent } = require<typeof CandidatesModule>("../src/features/mentions/candidates");
  const { HumanContextPanel, HumanRoomTimeline } = require<typeof HumanThreadsModule>("../src/features/threads/HumanThreads");
  const NewHumanConversation = require<typeof NewConvModule>("../src/features/threads/NewHumanConversation").default;
  const { NotificationToastProvider } = require<typeof NotificationToastModule>("../src/features/threads/NotificationToast");
  const { HomeMentionHandoffCard, findMentionedTeammates } = require<typeof HandoffModule>("../src/features/mentions/HomeMentionHandoff");
  const { ThreadComposerFrame } = require<typeof PartsModule>("../src/features/threads/ThreadChatParts");
  const { LocaleProvider, translateUiText } = require<typeof LocaleModule>("../src/lib/locale");
  const { allowPrivateReads, resetPrivateSession } = require<typeof SessionModule>("../src/lib/privateSession");
  const { setSessionKey } = require<typeof SessionKeyModule>("../src/lib/sessionKey");

  // 1. Parser, slugger, and /note detection
  equal(findActiveMention("@dev", 4), { start: 0, end: 4, query: "dev" }, "detects leading @mention");
  equal(findActiveMention("hey @be", 7), { start: 4, end: 7, query: "be" }, "detects mid-sentence @mention");
  equal(findActiveMention("user@example.com", 16), null, "ignores email addresses");
  equal(findActiveMention("mail alice.bob@domain.test", 26), null, "ignores dotted email prefix");
  equal(findActiveMention("@alice bob", 10), null, "stops once whitespace separates tokens");
  equal(startsWithNote("/note quiet"), true, "detects /note prefix");
  equal(startsWithNote("  /NOTE\tlog"), true, "detects /note case-insensitively");
  equal(startsWithNote("/notes plural"), false, "ignores non-/note command");
  equal(startsWithNote("send /note later"), false, "requires /note at start");
  equal(slugifyHandle("Mei Lin"), "mei-lin", "slugifies multi-word human name");
  equal(slugifyHandle("  Dev_2! "), "dev-2", "normalizes underscores and punctuation");

  const pickerCandidates: MentionsModule.MentionCandidate[] = [
    { id: "h-out", handle: "maya", label: "Maya", kind: "human", active: false, unavailableReason: "Not in this channel" },
    { id: "a-in", handle: "dev", label: "dev", kind: "agent", subtitle: "Builder", active: true },
    { id: "h-in", handle: "alice", label: "Alice", kind: "human", active: true },
  ];
  const filtered = filterMentionCandidates(pickerCandidates, "");
  equal(filtered.map((c) => c.handle), ["alice", "dev", "maya"], "ranks active members before unavailable hints");
  const m1 = findActiveMention("ask @de please", 7)!;
  equal(applyMention("ask @de please", m1, { handle: "dev" }), { text: "ask @dev please", caret: 8 }, "keeps existing space after mention");
  const m2 = findActiveMention("hi @de", 6)!;
  equal(applyMention("hi @de", m2, { handle: "dev" }), { text: "hi @dev ", caret: 8 }, "adds trailing space at end of input");

  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const flush = async () => {
    await React.act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  };

  // 2. MentionPicker grouped active + disabled unavailable rendering
  let picked: MentionsModule.MentionCandidate | null = null;
  await React.act(async () => {
    root.render(
      React.createElement(LocaleProvider, null,
        React.createElement(MentionPicker, {
          placement: "below",
          candidates: filtered,
          highlighted: 0,
          onHighlight: () => {},
          onSelect: (c) => { picked = c; },
        }),
      ),
    );
  });
  await flush();
  assert(host.querySelector(".mention-picker")?.getAttribute("data-placement") === "below", "MentionPicker supports placement='below'");
  assert(host.textContent?.includes("In this channel") && host.textContent?.includes("Not in channel"), "MentionPicker groups active and unavailable candidates");
  const disabledOpt = host.querySelector<HTMLButtonElement>('[aria-disabled="true"] button');
  assert(disabledOpt?.disabled === true, "unavailable candidate button is disabled");
  await React.act(async () => { disabledOpt?.click(); });
  assert(picked === null, "clicking disabled unavailable candidate does not select it");
  const activeOpt = host.querySelector<HTMLButtonElement>('[role="option"]:not([aria-disabled="true"]) button');
  await React.act(async () => { activeOpt?.click(); });
  assert((picked as MentionsModule.MentionCandidate | null)?.handle === "alice", "clicking active candidate selects it");

  // 3. Channels Context Panel + Timeline + Zero <select> + Searchable Select + ConfirmDialog
  const calls: { method: string; path: string; body: unknown }[] = [];
  const json = (data: unknown, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });

  const roomMembers: ClientModule.HumanMember[] = [
    { kind: "human", id: "human-me", label: "Mei" },
    { kind: "human", id: "human-bob", label: "Bob" },
    { kind: "human", id: "human-carol", label: "Carol" },
    { kind: "human", id: "human-dan", label: "Dan" },
    { kind: "human", id: "human-erin", label: "Erin" },
    { kind: "agent", id: "ag-dev", label: "dev", look: "builder" },
  ];
  const people: ClientModule.HumanPerson[] = [
    { id: "human-me", subject: "sub-me", name: "Mei" },
    { id: "human-bob", subject: "sub-bob", name: "Bob" },
    { id: "human-eva", subject: "sub-eva", name: "Eva" },
  ];
  const agents: ClientModule.HumanAgent[] = [
    { uid: "ag-dev", alias: "dev", look: "builder" },
    { uid: "ag-qa", alias: "qa", look: "tester" },
  ];
  const roomPage: ClientModule.HumanRoomPage = {
    room: "room-1",
    scope_id: "csrid_room1",
    members: roomMembers,
    posts: [
      {
        id: "post-1",
        read_cursor: "csc1_post1",
        sender: { id: "human-me", label: "Mei" },
        body: "/note internal check",
        created: 1700000100,
        silent: true,
        outcomes: [],
      },
      {
        id: "post-2",
        read_cursor: "csc1_post2",
        sender: { id: "human-bob", label: "Bob" },
        body: "@dev @maya status?",
        created: 1700000200,
        silent: false,
        outcomes: [
          { kind: "queued", handle: "@dev", label: "dev" },
          { kind: "blocked_dm", handle: "@maya", label: "maya" },
        ],
      },
      {
        id: "post-agent",
        read_cursor: "csc1_post_agent",
        sender: { id: "ag-dev", label: "dev", kind: "agent" },
        body: "Working on it now.",
        created: 1700000250,
        silent: false,
        outcomes: [],
      },
    ],
    events: [
      { id: "ev-1", text: "Mei added @dev", created: 1700000150 },
    ],
    before: "csc1_before",
    more_before: false,
    can_ask: true,
    can_manage: true,
    agent_chain_count: 2,
    agent_chain_limit: 2,
  };

  let holdRoomRead = false;
  let releaseRoomRead: (() => void) | null = null;

  globalThis.fetch = (async (input: unknown, init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost");
    const method = (init?.method ?? "GET").toUpperCase();
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    calls.push({ method, path: url.pathname, body });
    if (url.pathname === "/api/human-threads/stream") return new Response("", { status: 404 });
    if (url.pathname === "/api/human-threads/me") return json({ human: { id: "human-me", name: "Mei" } });
    if (url.pathname === "/api/human-threads/rooms") return json({ rooms: [roomSummary] });
    if (url.pathname === "/api/agents") return json({ daemon: "reachable", agents: [], totals: null, by_issue: {} });
    if (url.pathname === "/api/issues") return json({ issues: [] });
    if (url.pathname === "/api/human-threads/agents") return json({ agents });
    if (url.pathname === "/api/human-threads/people") {
      const q = (url.searchParams.get("query") ?? "").trim().toLowerCase();
      const filteredPeople = q ? people.filter((p) => p.name.toLowerCase().includes(q)) : people;
      return json({ actor: "human-me", people: filteredPeople, has_more: false });
    }
    if (url.pathname === "/api/human-threads/groups" && method === "POST") return json({ room: "room-new" });
    if (url.pathname === "/api/human-threads/rooms/room-1" && method === "GET") {
      if (holdRoomRead) {
        await new Promise<void>((resolve) => {
          releaseRoomRead = resolve;
        });
      }
      return json(roomPage);
    }
    if (url.pathname === "/api/human-threads/rooms/room-1/posts" && method === "POST") {
      return json({
        post: {
          id: "post-3",
          read_cursor: "csc1_post3",
          sender: { id: "human-me", label: "Mei" },
          body: (body as { body: string }).body,
          created: 1700000300,
          silent: Boolean((body as { silent?: boolean }).silent),
          outcomes: [],
        },
        created: true,
      });
    }
    if (url.pathname === "/api/human-threads/rooms/room-1/members" && method === "POST") return json({ ok: true });
    if (url.pathname.startsWith("/api/human-threads/rooms/room-1/members/") && method === "DELETE") return json({ ok: true });
    return json({});
  }) as typeof fetch;

  setSessionKey("mentions-channels-test");
  allowPrivateReads();

  const roomSummary: ClientModule.HumanRoomSummary = {
    id: "room-1",
    kind: "mixed_group",
    title: "Release Sync",
    creator: "human-me",
    created: 1700000000,
    unread: 0,
  };

  equal(
    [
      { alias: "dev", inbox: false, provider: "fake", dead: false, role: "worker", personal_assistant: false },
      { alias: "personal", inbox: false, provider: "pi", dead: false, role: "worker", personal_assistant: true },
      { alias: "assist-ana", inbox: false, provider: "pi", dead: false, role: "worker" },
      { alias: "master", inbox: false, provider: "pi", dead: false, role: "worker" },
      { alias: "rev-1", inbox: false, provider: "pi", dead: false, role: "reviewer" },
      { alias: "mail", inbox: false, provider: "inbox", dead: false, role: "worker" },
    ].filter((a) => isShareableAgent(a)).map((a) => a.alias),
    ["dev"],
    "isShareableAgent excludes personal_assistant, assist-*, master, reviewer, and inbox agents",
  );

  holdRoomRead = true;
  await React.act(async () => {
    root.render(
      React.createElement(LocaleProvider, null,
        React.createElement("div", null,
          React.createElement(HumanRoomTimeline, {
            id: "room-1",
            me: { id: "human-me", name: "Mei" },
            refresh: 0,
            onState: () => {},
            onOpenContext: () => {},
            detailsOpen: true,
            onInvite: () => {},
            onChanged: () => {},
            isDm: false,
            roomTitle: "Release Sync",
            people,
          }),
          React.createElement(HumanContextPanel, {
            room: roomSummary,
            me: { id: "human-me", name: "Mei" },
            members: roomMembers,
            denied: false,
            canManage: true,
            agentChainCount: 2,
            agentChainLimit: 2,
            people,
            peopleError: null,
            agents,
            onChanged: () => {},
            onOpened: () => {},
          }),
          React.createElement(ThreadComposerFrame, {
            allowAsk: true,
            mode: "ask",
            target: "dev",
            targetOptions: [{ id: "dev", label: "dev" }],
            children: null,
          }),
        ),
      ),
    );
  });
  await flush();
  assert(host.querySelector('.threads-skel-timeline[role="status"][aria-busy="true"]') !== null, "HumanRoomTimeline shows skeleton while room fetch is pending");
  holdRoomRead = false;
  (releaseRoomRead as (() => void) | null)?.();
  for (let i = 0; i < 6; i += 1) await flush();

  assert(host.querySelector('.threads-skel-timeline') === null, "HumanRoomTimeline clears skeleton once room loads");
  assert(host.querySelector('[data-post-id="post-agent"] .agent-avatar[data-slug="builder"]') !== null, "Agent message displays pinned AgentAvatar in chat conversation");
  assert(host.querySelector('.threads-roster [data-kind="agent"] .agent-avatar[data-slug="builder"]') !== null, "Details roster displays AgentAvatar for shared agents");
  assert(host.querySelector('.threads-context-room')?.textContent?.includes("Release Sync"), "Details panel displays compact room summary header");
  assert(host.querySelector('details.threads-guardrails-card')?.hasAttribute("open"), "Active Guardrails disclosure defaults open when paused");

  assert(host.querySelectorAll("select").length === 0, "Channels and ThreadComposerFrame render zero native <select> elements");
  assert(
    host.querySelector(".threads-context-toggle")?.getAttribute("aria-expanded") === "true",
    "Details button forwards aria-expanded to DOM Button element",
  );
  assert(host.querySelector(".threads-composer-dock") !== null, "floating composer dock is rendered");
  assert(host.textContent?.includes("Silent note"), "silent post displays Silent note tag");
  assert(host.textContent?.includes("@dev · queued") && host.textContent?.includes("@maya · blocked in DM"), "renders server mention outcome pills");
  assert(host.textContent?.includes("Mei added @dev"), "renders interleaved channel audit event");
  assert(host.textContent?.includes("Channel paused after consecutive agent turns"), "renders Human-Turn Brake banner at chain limit");

  // Split Context sections: Teammates, Shared Agents, and Active Guardrails
  assert(host.querySelector('[data-roster-humans]') !== null && host.querySelector('[data-roster-agents]') !== null, "HumanContextPanel separates Teammates and Shared Agents");
  const guardrailsText = host.querySelector("[data-active-guardrails]")?.textContent ?? "";
  assert(
    guardrailsText.includes("Membership ACL")
      && guardrailsText.includes("Paused after consecutive agent turns until a human posts.")
      && !guardrailsText.includes("Agent hops: 2/2"),
    "Active Guardrails shows paused state when agent chain hits limit",
  );

  // Searchable member filter (>= 5 human members)
  const filterInput = host.querySelector<HTMLInputElement>(".threads-member-filter");
  assert(filterInput !== null, "member search filter shows when channel has >= 5 teammates");
  const inputSetter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!;
  await React.act(async () => {
    inputSetter.call(filterInput!, "Carol");
    filterInput!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
  const visibleRosterText = host.querySelector("[data-roster-humans]")?.textContent ?? "";
  assert(visibleRosterText.includes("Carol") && !visibleRosterText.includes("Dan"), "live member filter narrows teammate list");
  await React.act(async () => {
    inputSetter.call(filterInput!, "");
    filterInput!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();

  // Searchable <Select> for adding person Eva
  const personCombo = host.querySelector<HTMLButtonElement>('[aria-label="Add a person"][role="combobox"]');
  assert(personCombo !== null, "Add a person uses searchable <Select> combobox");
  await React.act(async () => { personCombo!.click(); });
  await flush();
  assert(document.querySelector(".select-pop .select-search") !== null, "Select renders searchable input in popup");
  const evaOpt = Array.from(document.querySelectorAll<HTMLElement>(".select-opt")).find((el) => el.textContent?.includes("Eva"));
  assert(evaOpt !== undefined, "Select lists addable person Eva");
  await React.act(async () => { evaOpt!.click(); });
  await flush();
  const addPersonBtn = host.querySelector<HTMLButtonElement>('form[aria-label="Invite teammate to channel"] button[type="submit"]');
  assert(addPersonBtn !== null && !addPersonBtn.disabled, "Add person submits once Eva is chosen");
  await React.act(async () => { addPersonBtn!.click(); });
  await flush();
  assert(calls.some((c) => c.method === "POST" && c.path === "/api/human-threads/rooms/room-1/members" && (c.body as { human_subject?: string }).human_subject === "sub-eva"), "Add person posts sub-eva to members endpoint");

  // Remove ConfirmDialog: role=alertdialog, target card, Cancel focus default + Escape keep member; Confirm sends DELETE
  const bobRow = Array.from(host.querySelectorAll<HTMLElement>("[data-roster-humans] li")).find((li) => li.textContent?.includes("Bob"));
  const bobRemove = bobRow?.querySelector<HTMLButtonElement>("button");
  assert(bobRemove !== null, "Bob has Remove action for channel manager");
  await React.act(async () => { bobRemove!.click(); });
  await flush();
  const confirmEl = host.querySelector<HTMLElement>(".s-confirm");
  assert(confirmEl?.getAttribute("role") === "alertdialog", "Remove ConfirmDialog uses role=alertdialog");
  assert(host.querySelector(".threads-remove-card")?.textContent?.includes("Bob"), "Remove ConfirmDialog summarizes target person");
  const cancelBtn = Array.from(host.querySelectorAll<HTMLButtonElement>(".s-confirm button")).find((b) => b.textContent?.trim() === "Cancel");
  assert(document.activeElement === cancelBtn, "Remove ConfirmDialog focuses Cancel by default");
  await React.act(async () => { cancelBtn?.click(); });
  await flush();
  assert(host.querySelector(".s-confirm-wrap") === null, "Cancel closes ConfirmDialog");
  assert(!calls.some((c) => c.method === "DELETE"), "Cancel sends no DELETE request");

  await React.act(async () => { bobRemove!.click(); });
  await flush();
  await React.act(async () => {
    host.querySelector(".s-confirm")?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  });
  await flush();
  assert(host.querySelector(".s-confirm-wrap") === null, "Escape closes ConfirmDialog");
  assert(!calls.some((c) => c.method === "DELETE"), "Escape sends no DELETE request");

  await React.act(async () => { bobRemove!.click(); });
  await flush();
  const confirmBtn = Array.from(host.querySelectorAll<HTMLButtonElement>(".s-confirm button")).find((b) => b.textContent?.trim() === "Remove");
  await React.act(async () => { confirmBtn?.click(); });
  await flush();
  assert(calls.some((c) => c.method === "DELETE" && c.path === "/api/human-threads/rooms/room-1/members/human/human-bob"), "Confirm invokes DELETE /members/human/human-bob");

  // /note toggle + @mention autocomplete inside Channels composer
  const noteBtn = host.querySelector<HTMLButtonElement>(".composer-silent-toggle");
  const textarea = host.querySelector<HTMLTextAreaElement>("textarea.app-chat-textarea");
  assert(noteBtn !== null && textarea !== null, "Channels composer renders /note toggle and textarea");
  const taSetter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, "value")!.set!;
  await React.act(async () => {
    taSetter.call(textarea!, "check @de");
    textarea!.selectionStart = 9;
    textarea!.selectionEnd = 9;
    textarea!.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await flush();
  assert(host.querySelector(".mention-picker") !== null, "typing @de opens MentionPicker in Channels composer");
  await React.act(async () => {
    textarea!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });
  await flush();
  assert(textarea!.value === "check @dev ", "Enter completes active @mention candidate");
  await React.act(async () => { noteBtn!.click(); });
  await flush();
  assert(noteBtn!.getAttribute("aria-pressed") === "true", "/note toggle marks aria-pressed=true");
  await React.act(async () => {
    textarea!.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  });
  await flush();
  assert(
    calls.some((c) => c.method === "POST" && c.path === "/api/human-threads/rooms/room-1/posts" && (c.body as { silent?: boolean; body?: string }).silent === true),
    "submitting with /note toggled sends silent: true",
  );

  // 4. NewHumanConversation: DM badge + searchable multi-pick Select without checkboxes
  let createdRoom: string | null = null;
  await React.act(async () => {
    root.render(
      React.createElement(LocaleProvider, null,
        React.createElement("div", null,
          React.createElement(NewHumanConversation, {
            mode: "direct",
            me: { id: "human-me", name: "Mei" },
            agents,
            agentsError: null,
            existingDmPeers: new Set(["bob"]),
            onBusy: () => {},
            onCancel: () => {},
            onCreated: (id) => { createdRoom = id; },
          }),
          React.createElement(NewHumanConversation, {
            mode: "group",
            me: { id: "human-me", name: "Mei" },
            agents,
            agentsError: null,
            onBusy: () => {},
            onCancel: () => {},
            onCreated: (id) => { createdRoom = id; },
          }),
        ),
      ),
    );
  });
  for (let i = 0; i < 6; i += 1) await flush();
  assert(host.querySelectorAll('input[type="checkbox"], input[type="radio"]').length === 0, "NewHumanConversation uses zero checkboxes or radios");
  assert(!host.textContent?.includes("Scope") && !host.textContent?.includes("Project"), "NewHumanConversation omits redundant Scope & Project fields");
  const dmCombo = host.querySelector<HTMLButtonElement>('form[aria-label="New direct message"] [role="combobox"]');
  await React.act(async () => { dmCombo?.click(); });
  await flush();
  const bobDmOpt = Array.from(document.querySelectorAll<HTMLElement>(".select-opt")).find((el) => el.textContent?.includes("Bob"));
  assert(bobDmOpt?.textContent?.includes("Existing DM"), "DM dropdown flags existing direct message peers");
  await React.act(async () => { bobDmOpt?.click(); });
  await flush();

  const chCombo = host.querySelector<HTMLButtonElement>('form[aria-label="New channel"] [aria-label="Teammates"][role="combobox"]');
  await React.act(async () => { chCombo?.click(); });
  await flush();
  const searchInput = document.querySelector<HTMLInputElement>(".select-pop .select-search");
  const bobGroupOpt = Array.from(document.querySelectorAll<HTMLElement>(".select-opt")).find((el) => el.textContent?.includes("Bob"));
  await React.act(async () => { bobGroupOpt?.click(); });
  await flush();
  assert(document.querySelector(".select-pop") !== null && document.activeElement === searchInput, "keepOpenOnPick preserves popup open state and search focus");
  const evaGroupOpt = Array.from(document.querySelectorAll<HTMLElement>(".select-opt")).find((el) => el.textContent?.includes("Eva"));
  await React.act(async () => { evaGroupOpt?.click(); });
  await flush();
  assert(host.querySelectorAll(".threads-picked .threads-picked-person").length === 2, "multi-pick adds removable teammate chips");
  const createGroupBtn = host.querySelector<HTMLButtonElement>('form[aria-label="New channel"] button[type="submit"]');
  await React.act(async () => { createGroupBtn?.click(); });
  await flush();
  assert(
    calls.some((c) => c.method === "POST" && c.path === "/api/human-threads/groups" && (c.body as { kind?: string }).kind === "human_group") &&
      createdRoom === "room-new",
    "NewHumanConversation submits new channel creation",
  );

  // 5. Private Home colleague mention detection and consent-gated HomeMentionHandoffCard
  const homeCandidates: MentionsModule.MentionCandidate[] = [
    { id: "human-bob", handle: "bob", label: "Bob", kind: "human", active: true },
  ];
  equal(findMentionedTeammates("ping @bob on release", homeCandidates).map((c) => c.handle), ["bob"], "finds non-silent @teammate in Home draft");
  equal(findMentionedTeammates("email bob@example.com", homeCandidates), [], "ignores email addresses in Home draft");
  equal(findMentionedTeammates("/note private @bob", homeCandidates), [], "ignores /note silent drafts in Home");

  let handoffClicks = 0;
  let keepPrivateClicks = 0;
  await React.act(async () => {
    root.render(
      React.createElement(LocaleProvider, null,
        React.createElement(HomeMentionHandoffCard, {
          teammates: homeCandidates,
          sendBlockedForConsent: true,
          onOpenChannels: () => { handoffClicks += 1; },
          onKeepPrivate: () => { keepPrivateClicks += 1; },
        }),
      ),
    );
  });
  await flush();
  const handoffCard = host.querySelector<HTMLElement>("[data-home-handoff-preview]");
  assert(handoffCard?.getAttribute("role") === "alert", "held teammate mention in Home surfaces role=alert consent banner");
  const openChannelsBtn = Array.from(host.querySelectorAll<HTMLButtonElement>("[data-home-handoff-preview] button")).find((b) => b.textContent?.includes("Open in Channels"));
  const keepPrivateBtn = Array.from(host.querySelectorAll<HTMLButtonElement>("[data-home-handoff-preview] button")).find((b) => b.textContent?.includes("Keep private in Home"));
  await React.act(async () => { openChannelsBtn?.click(); keepPrivateBtn?.click(); });
  assert(handoffClicks === 1 && keepPrivateClicks === 1, "HomeMentionHandoffCard wires Open in Channels and Keep private in Home actions");

  // 6. Human-Turn Brake 3-state branch in HumanContextPanel + NotificationToast CSS stacking
  for (const [chainCount, chainLimit, expectedText, forbiddenText] of [
    [0, 0, "Inactive until a shared agent joins this channel.", "Paused after consecutive agent turns until a human posts."],
    [1, 2, "Agent hops: 1/2 before human turn required.", "Paused after consecutive agent turns until a human posts."],
    [2, 2, "Paused after consecutive agent turns until a human posts.", "Agent hops: 2/2"],
  ] as const) {
    await React.act(async () => {
      root.render(
        React.createElement(LocaleProvider, null,
          React.createElement(NotificationToastProvider, null,
            React.createElement(HumanContextPanel, {
              room: {
                id: "room-brake",
                kind: chainLimit === 0 ? "human_group" : "mixed_group",
                title: "Brake State",
                created: 1,
                creator: "human-me",
                unread: 0,
              },
              me: { id: "human-me", name: "Mei" },
              members: [
                { kind: "human", id: "human-me", label: "Mei" },
                ...(chainLimit > 0 ? [{ kind: "agent" as const, id: "uid-dev", label: "dev" }] : []),
              ],
              denied: false,
              canManage: true,
              agentChainCount: chainCount,
              agentChainLimit: chainLimit,
              people,
              peopleError: null,
              agents,
              onChanged: () => {},
              onOpened: () => {},
            }),
          ),
        ),
      );
    });
    await flush();
    const brakeText = host.querySelector("[data-active-guardrails]")?.textContent ?? "";
    assert(brakeText.includes(expectedText) && !brakeText.includes(forbiddenText), `Human-Turn Brake renders expected state for (${chainCount}/${chainLimit})`);
    const toastStack = host.querySelector<HTMLElement>(".notification-toast-stack");
    assert(toastStack !== null && toastStack.style.display === "", "NotificationToastStack delegates display to CSS without inline display override");
  }

  // 7. CAD-2736 & CAD-2765 Channels zh-TW locale entries
  equal(
    [
      translateUiText("Close new conversation", "zh-TW"),
      translateUiText("Channels", "zh-TW"),
      translateUiText("Channels are unavailable for this sign-in.", "zh-TW"),
      translateUiText("No channels yet.", "zh-TW"),
    ],
    ["關閉新對話", "頻道", "此登入帳號無法使用頻道。", "尚無頻道。"],
    "zh-TW Channels and modal close copy translate cleanly",
  );

  // 8. CAD-2765 ThreadsPage on hosted boards (workConversationsEnabled=false) clears loading state without stuck 'Loading conversations…'
  const ThreadsPage = require<{ default: ReactModule.ComponentType<{ operator: boolean; workConversationsEnabled?: boolean; humanThreadsEnabled?: boolean }> }>("../src/features/threads/ThreadsPage").default;
  await React.act(async () => {
    root.render(
      React.createElement(LocaleProvider, null,
        React.createElement(ThreadsPage, {
          operator: true,
          workConversationsEnabled: false,
          humanThreadsEnabled: true,
        }),
      ),
    );
  });
  for (let i = 0; i < 8; i += 1) await flush();
  const sidebarText = host.querySelector(".threads-list")?.textContent ?? "";
  assert(
    sidebarText.includes("Release Sync") && !sidebarText.includes("Loading conversations…"),
    "Hosted Channels sidebar does not get stuck displaying 'Loading conversations…' above channels",
  );

  await React.act(async () => root.unmount());
  host.remove();
  resetPrivateSession();
  console.log("mentions and channels checks passed");
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
