import { Window } from "happy-dom";

const win = new Window({ url: "http://127.0.0.1/" });
Object.assign(globalThis, {
  window: win,
  document: win.document,
  Element: win.Element,
  HTMLElement: win.HTMLElement,
  HTMLDialogElement: (win as unknown as { HTMLDialogElement?: unknown }).HTMLDialogElement ?? win.HTMLElement,
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
import HireSheet from "../src/features/agents/HireSheet";
import { translateUiText } from "../src/lib/locale";
import type { AgentRole, AgentRolesPayload } from "../src/lib/types";

declare const process: { exit(code?: number): never };

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`hireSheet: ${msg}`);
}

const flush = () => new Promise((r) => setTimeout(r, 30));

async function main() {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/api/apps")) {
      return new Response(
        JSON.stringify({
          apps: [{ name: "social-content", title: "Social Content", project: "main", screens: ["main"] }],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (url.includes("/api/agents/settings")) {
      return new Response(
        JSON.stringify({ config: { models: [] } }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;

  const role: AgentRole = {
    id: "digital-marketer",
    category: "marketing",
    title: "Digital Marketer",
    look: "content",
    team_role: "worker",
    kind: "apps",
    job: "Drafts social posts and audits ads spend.",
    does: ["Drafts campaign posts"],
    skills: ["company-outputs"],
    tools: ["Social Content app (read, draft)", "Ads Studio app (read, stage proposals)"],
    first_tasks: ["Audit weekly spend"],
    instructions: "You are a Digital Marketer.",
    recommended_apps: [
      { app_key: "social-content", access: "draft", default_bind: true },
      { app_key: "ads-studio", access: "draft", default_bind: true },
      { app_key: "calendar", access: "read", default_bind: false },
    ],
    disabled_tools: ["mail", "ops"],
    context_seed: "# Marketing Playbook",
  };
  const payload: AgentRolesPayload = {
    categories: [{ id: "marketing", label: "Marketing" }],
    roles: [role],
    install: { hosted: true, provider: "pi", endpoint_kind: "durable_object" },
  };

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  await React.act(async () => {
    root.render(
      <HireSheet
        role={role}
        payload={payload}
        alias="marketer-1"
        taken={[]}
        onClose={() => undefined}
        onHired={() => undefined}
      />,
    );
    await flush();
  });

  const text = document.body.textContent ?? "";
  assert(text.includes("Marketing"), "shows Marketing category label");
  assert(text.includes("Recommended apps"), "shows Recommended apps heading");
  assert(text.includes("social-content · Read & draft · Installed"), "shows installed app status");
  assert(text.includes("ads-studio · Read & draft · Install later"), "shows install-later app status");
  assert(text.includes("calendar · Read-only · Install later"), "shows read-only access label");
  assert(text.includes("Starter playbook seeded at hire"), "shows starter playbook chip");
  assert(text.includes("Disabled tool groups: mail, ops"), "shows disabled tool groups note");
  assert(!text.includes("planned"), "does not display stale planned chip");

  // Verify zh-TW translations for all CAD-2857 HireSheet labels.
  for (const label of [
    "Marketing",
    "Recommended apps",
    "Read & draft",
    "Read-only",
    "Installed",
    "Install later",
    "Starter playbook seeded at hire",
    "Owner approval required before external sends",
  ]) {
    assert(translateUiText(label, "zh-TW") !== label, `missing zh-TW translation for ${label}`);
  }

  root.unmount();
  console.log("hireSheet checks passed");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
