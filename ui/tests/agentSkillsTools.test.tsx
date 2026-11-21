import { Window } from "happy-dom";

const win = new Window({ url: "http://127.0.0.1/" });
Object.assign(globalThis, {
  window: win,
  document: win.document,
  Element: win.Element,
  HTMLElement: win.HTMLElement,
  SVGElement: win.SVGElement,
  Node: win.Node,
  Event: win.Event,
  MouseEvent: win.MouseEvent,
  KeyboardEvent: win.KeyboardEvent,
  CustomEvent: win.CustomEvent,
  MutationObserver: win.MutationObserver,
  addEventListener: win.addEventListener.bind(win),
  removeEventListener: win.removeEventListener.bind(win),
  IS_REACT_ACT_ENVIRONMENT: true,
});

import React from "react";
import { createRoot } from "react-dom/client";
import AgentTabBody, { type TabContext } from "../src/features/agents/AgentTabs";
import type { AgentDetail, NativeSkillItem, NativeToolItem } from "../src/lib/types";
import type { InstructionsDoc } from "../src/features/agents/useInstructionsDoc";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

const SKILLS: NativeSkillItem[] = [
  {
    name: "cadence",
    line: "Tickets, threads and reports",
    source: "bundled",
    bundled: true,
    required: true,
    enabled: true,
    loaded: true,
    applies: "next_launch",
    skill_md: "# cadence\nRequired skill body.\n",
    files: ["SKILL.md"],
  },
  {
    name: "deliverable-qa",
    line: "Independent browser and visual QA before release",
    source: "bundled",
    bundled: true,
    required: false,
    enabled: true,
    loaded: false,
    applies: "next_launch",
    skill_md: "# deliverable-qa\nQA instructions.\n",
    files: ["SKILL.md"],
  },
  {
    name: "website-builder",
    line: "Build and publish a static website",
    source: "bundled",
    bundled: true,
    required: false,
    enabled: false,
    loaded: null,
    applies: "next_launch",
    skill_md: "# website-builder\nSite builder.\n",
    files: ["SKILL.md", "playbook.md"],
  },
];

const TOOLS: NativeToolItem[] = [
  {
    name: "cadence",
    line: "Tickets, threads and reports",
    description: "Run cadence CLI for tickets, threads, messages and reports.",
    source: "built_in",
    bundled: true,
    required: true,
    enabled: true,
    applies: "next_launch",
  },
  {
    name: "write",
    line: "Create or overwrite files",
    description: "Write new files or replace file contents.",
    source: "built_in",
    bundled: true,
    required: false,
    enabled: true,
    applies: "next_launch",
  },
];

const STUB_DOC: InstructionsDoc = {
  kind: "native",
  read: "ready",
  served: "Job text",
  draft: "Job text",
  setDraft: () => {},
  phase: "idle",
  profile: null,
  applies: null,
  working: null,
  selection: { current: { start: 0, end: 0, top: 0 } },
  retry: () => {},
  save: () => {},
  saveMine: () => {},
  useTheirs: () => {},
  check: () => {},
  discard: () => {},
  saveSkills: async () => ({ kind: "refused", message: "" }),
  saveEnabledTools: async () => ({ kind: "refused", message: "" }),
};

function makeCtx(overrides: Partial<TabContext> = {}): TabContext {
  const detail: AgentDetail = {
    agent: {
      alias: "dev-1",
      provider: "pi",
      endpoint_kind: "managed",
      state: "idle",
      enabled: true,
      loaded_skills: ["cadence"],
    },
    queued: 0,
    unknown: 0,
    running: [],
    events: [],
    tasks: [],
    on: [],
    native_skills: SKILLS,
    native_tools: TOOLS,
  };
  return {
    alias: "dev-1",
    profile: undefined,
    observed: undefined,
    detail,
    role: null,
    hosted: false,
    hostedBoard: false,
    resume: null,
    onOpenIssue: () => {},
    hired: undefined,
    operator: true,
    writable: true,
    readOnlyReason: null,
    toast: () => {},
    stats: [],
    detailFailed: false,
    detailNotServed: false,
    retryDetail: () => {},
    roleRead: "read",
    retryRole: () => {},
    doc: STUB_DOC,
    ...overrides,
  };
}

async function main() {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const posts: { url: string; body: unknown }[] = [];
  const toasts: { kind: string; text: string }[] = [];
  let retries = 0;

  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const body = init?.body ? JSON.parse(String(init.body)) : null;
    posts.push({ url, body });
    if (url.endsWith("/skills")) {
      return new Response(
        JSON.stringify({ alias: "dev-1", state: "updated", applies: "next_launch", skills: body.skills }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (url.endsWith("/tools")) {
      return new Response(
        JSON.stringify({
          alias: "dev-1",
          state: "updated",
          applies: "next_launch",
          disabled_tools: body.disabled_tools,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;

  // 1. Native Skills tab: required switch disabled, Not loaded badge from evidence, drawer shows SKILL.md, toggle posts & toasts.
  await React.act(async () => {
    root.render(
      React.createElement(AgentTabBody, {
        tab: "skills",
        c: makeCtx({
          toast: (kind, text) => toasts.push({ kind, text }),
          retryDetail: () => {
            retries += 1;
          },
        }),
      }),
    );
  });

  const cadenceSwitch = host.querySelector<HTMLButtonElement>('button[role="switch"][aria-label="On for this agent: cadence"]');
  assert(cadenceSwitch, "cadence skill switch renders");
  assert(cadenceSwitch.disabled && cadenceSwitch.getAttribute("aria-checked") === "true", "required cadence skill cannot be turned off");

  const qaSwitch = host.querySelector<HTMLButtonElement>('button[role="switch"][aria-label="On for this agent: deliverable-qa"]');
  assert(qaSwitch && !qaSwitch.disabled && qaSwitch.getAttribute("aria-checked") === "true", "bundled non-required skill switch is toggleable");
  assert(host.textContent?.includes("Not loaded"), "enabled skill absent from runtime loaded_skills shows Not loaded chip");

  // Open deliverable-qa drawer: proves SKILL.md and honest runtime evidence (never "In every session").
  const qaRow = host.querySelector<HTMLButtonElement>('[data-record-id="skill-deliverable-qa"]');
  assert(qaRow, "deliverable-qa row exists");
  await React.act(async () => {
    qaRow.click();
  });
  const drawer = host.querySelector("[data-drawer='agent-item']");
  assert(drawer?.textContent?.includes("QA instructions."), "drawer shows SKILL.md content");
  assert(drawer?.textContent?.includes("Not loaded in running session"), "drawer shows runtime loaded=false honestly");
  assert(!drawer?.textContent?.includes("In every session"), "never fabricates In every session");
  assert(!drawer?.textContent?.includes("Delete"), "bundled skill offers no Delete action");

  // Toggle deliverable-qa off -> POST /api/agents/dev-1/skills with ["cadence"] and toast Applies next session.
  await React.act(async () => {
    qaSwitch.click();
  });
  const skillPost = posts.shift();
  assert(skillPost && skillPost.url === "/api/agents/dev-1/skills", "toggle posts to /api/agents/dev-1/skills");
  assert(JSON.stringify(skillPost.body) === JSON.stringify({ skills: ["cadence"] }), "only enabled skills sent");
  assert(retries === 1, "detail refreshed without restart");
  assert(toasts.some((t) => t.text === "Saved. Applies next session."), "toasts next-session apply notice");

  // 2. Native Tools tab: required cadence disabled, toggle write off -> POST /api/agents/dev-1/tools.
  await React.act(async () => {
    root.render(
      React.createElement(AgentTabBody, {
        tab: "tools",
        c: makeCtx({
          toast: (kind, text) => toasts.push({ kind, text }),
        }),
      }),
    );
  });
  const cadenceTool = host.querySelector<HTMLButtonElement>('button[role="switch"][aria-label="On for this agent: cadence"]');
  assert(cadenceTool?.disabled && cadenceTool.getAttribute("aria-checked") === "true", "required cadence tool cannot be turned off");
  const writeTool = host.querySelector<HTMLButtonElement>('button[role="switch"][aria-label="On for this agent: write"]');
  assert(writeTool && !writeTool.disabled, "built-in write tool switch is toggleable");
  await React.act(async () => {
    writeTool.click();
  });
  const toolPost = posts.shift();
  assert(toolPost && toolPost.url === "/api/agents/dev-1/tools", "tool toggle posts to /api/agents/dev-1/tools");
  assert(JSON.stringify(toolPost.body) === JSON.stringify({ disabled_tools: ["write"] }), "disabled_tools payload matches");

  // 3. Read-only / member view renders zero toggleable switches.
  await React.act(async () => {
    root.render(
      React.createElement(AgentTabBody, {
        tab: "skills",
        c: makeCtx({ operator: false, writable: false }),
      }),
    );
  });
  const enabledSwitches = Array.from(host.querySelectorAll<HTMLButtonElement>('button[role="switch"]')).filter((b) => !b.disabled);
  assert(enabledSwitches.length === 0, "member/read-only view exposes no writable switches");

  root.unmount();
  console.log("agentSkillsTools checks passed");
}

void main();
