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
import { profileBody, type DocDeps } from "../src/features/agents/instructionsDoc";
import { toolGroupOn } from "../src/features/agents/profileTools";
import { useInstructionsDoc, type InstructionsDoc } from "../src/features/agents/useInstructionsDoc";
import { ApiError } from "../src/lib/apiError";
import type { StoredAgentProfile } from "../src/lib/types";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

const profile = (extra: Partial<StoredAgentProfile> = {}): StoredAgentProfile => ({
  id: "ap_1",
  name: "Dev",
  role: "dev",
  responsibilities: "Job text",
  skills: ["a"],
  status: "active",
  revision: 3,
  created_at: 1,
  updated_at: 1,
  created_by: "owner",
  catalogue: null,
  model: "m",
  ...extra,
});

async function main() {
  // Display rule: an opt-in group is on only when enabled_tools names it; disabled_tools keeps its old meaning.
  assert(!toolGroupOn(profile(), "cadence_writes"), "absent enabled_tools shows Cadence writes off");
  assert(!toolGroupOn(profile({ disabled_tools: [] }), "cadence_writes"), "not being in disabled_tools does not turn an opt-in group on");
  assert(toolGroupOn(profile({ enabled_tools: ["cadence_writes"] }), "cadence_writes"), "enabled_tools turns it on");
  for (const wabaGroup of ["whatsapp_read", "whatsapp_stage", "whatsapp_manage"]) {
    assert(!toolGroupOn(profile(), wabaGroup), `absent enabled_tools shows ${wabaGroup} off`);
    assert(!toolGroupOn(profile({ disabled_tools: ["board"] }), wabaGroup), `not being in disabled_tools never turns ${wabaGroup} on`);
    assert(toolGroupOn(profile({ enabled_tools: [wabaGroup] }), wabaGroup), `enabled_tools turns ${wabaGroup} on`);
  }
  assert(toolGroupOn(profile(), "board"), "an ordinary group is on unless disabled");
  assert(!toolGroupOn(profile({ disabled_tools: ["board"] }), "board"), "an ordinary group listed in disabled_tools is off");
  // The update body sends enabled_tools only when asked.
  assert(!("enabled_tools" in profileBody(profile(), "t", 3)), "enabled_tools is omitted unless changed");

  const host = document.createElement("div");
  document.body.appendChild(host);
  const root = createRoot(host);
  const sent: string[][] = [];
  const toasts: string[] = [];
  const mount = async (p: StoredAgentProfile, writable: boolean) => {
    const c = {
      alias: "ap_1",
      hired: { id: "ap_1", alias: "dev" },
      hostedBoard: true,
      role: null,
      roleRead: "read",
      writable,
      toast: (_k: string, text: string) => toasts.push(text),
      doc: {
        kind: "hosted",
        read: "ready",
        profile: p,
        saveEnabledTools: async (tools: string[]) => {
          sent.push(tools);
          return { kind: "saved" };
        },
      },
    } as unknown as TabContext;
    await React.act(async () => {
      root.render(React.createElement(AgentTabBody, { tab: "tools", c }));
    });
  };
  const sw = (label = "Cadence writes") =>
    host.querySelector<HTMLButtonElement>(`button[role="switch"][aria-label="On for this agent: ${label}"]`);

  await mount(profile(), true);
  assert(host.textContent?.includes("Cadence writes"), "the toggle is listed");
  for (const label of ["WhatsApp read", "WhatsApp draft & stage", "WhatsApp lifecycle"]) {
    assert(host.textContent?.includes(label), `${label} toggle is listed`);
    assert(sw(label)?.getAttribute("aria-checked") === "false", `${label} defaults to off`);
  }
  assert(sw()?.getAttribute("aria-checked") === "false", "a profile with no enabled_tools shows it off");
  await React.act(async () => {
    sw()!.click();
  });
  assert(JSON.stringify(sent) === JSON.stringify([["cadence_writes"]]), "an owner turning it on saves enabled_tools: [cadence_writes]");

  await React.act(async () => {
    sw("WhatsApp read")!.click();
  });
  assert(JSON.stringify(sent[sent.length - 1]) === JSON.stringify(["whatsapp_read"]), "an owner can toggle WhatsApp read");

  await mount(profile({ enabled_tools: ["cadence_writes", "whatsapp_read", "whatsapp_stage"] }), true);
  assert(sw("WhatsApp read")?.getAttribute("aria-checked") === "true", "WhatsApp read reflects enabled_tools");
  assert(sw("WhatsApp draft & stage")?.getAttribute("aria-checked") === "true", "WhatsApp draft & stage reflects enabled_tools");
  assert(sw("WhatsApp lifecycle")?.getAttribute("aria-checked") === "false", "WhatsApp lifecycle stays off");
  await React.act(async () => {
    sw("WhatsApp lifecycle")!.click();
  });
  assert(
    JSON.stringify(sent[sent.length - 1]) === JSON.stringify(["cadence_writes", "whatsapp_read", "whatsapp_stage", "whatsapp_manage"]),
    "toggling WhatsApp lifecycle preserves existing enabled_tools",
  );

  await mount(profile({ enabled_tools: ["cadence_writes"] }), true);
  assert(sw()?.getAttribute("aria-checked") === "true", "the toggle reflects enabled_tools");
  await React.act(async () => {
    sw()!.click();
  });
  assert(JSON.stringify(sent[sent.length - 1]) === "[]", "an owner turning it off saves an empty enabled_tools");

  await mount(profile({ enabled_tools: ["cadence_writes", "whatsapp_manage"] }), false);
  assert(sw() === null && sw("WhatsApp read") === null && sw("WhatsApp lifecycle") === null, "a non-owner gets no switch");
  const states = Array.from(host.querySelectorAll('[data-work-panel="opt-in-tools"] .sk-switch')).map((el) =>
    `${el.getAttribute("data-state")}:${el.textContent}`,
  );
  assert(
    JSON.stringify(states) === JSON.stringify(["on:On", "off:Off", "off:Off", "on:On"]),
    `a non-owner still sees all opt-in states read-only: ${JSON.stringify(states)}`,
  );
  assert(sent.length === 4, "a non-owner sends nothing");

  // The real hook: body is the stored fields plus enabled_tools at the page's revision; a 409 is a conflict.
  const writes: object[] = [];
  let fail: Error | null = null;
  const stored = profile({ disabled_tools: ["board"] });
  let doc: InstructionsDoc | null = null;
  const deps: DocDeps = {
    readProfile: async () => stored,
    writeProfile: async (_id, body) => {
      writes.push(body);
      if (fail) throw fail;
      return { ...stored, revision: 4, enabled_tools: (body as { enabled_tools: string[] }).enabled_tools };
    },
    readNative: async () => null,
    writeNative: async () => ({ applies: "next_turn" }),
  };
  function Harness() {
    doc = useInstructionsDoc({
      alias: "dev",
      kind: "hosted",
      profileId: "ap_1",
      nativeServed: null,
      writable: true,
      onSaved: () => {},
      toast: () => {},
      deps,
    });
    return null;
  }
  await React.act(async () => {
    root.render(React.createElement(Harness));
  });
  await React.act(async () => {
    await new Promise((r) => setTimeout(r, 20));
  });
  let out = await React.act(async () => doc!.saveEnabledTools(["cadence_writes"]));
  assert(out.kind === "saved", "the save lands");
  assert(
    JSON.stringify(writes[0]) ===
      JSON.stringify({ name: "Dev", role: "dev", responsibilities: "Job text", skills: ["a"], status: "active", expectedRevision: 3, enabled_tools: ["cadence_writes"] }),
    `body is the stored fields plus enabled_tools at the held revision: ${JSON.stringify(writes[0])}`,
  );
  fail = new ApiError("Reload the agent profile.", 409);
  out = await React.act(async () => doc!.saveEnabledTools([]));
  assert(out.kind === "conflict", "a 409 maps to conflict");

  root.unmount();
  console.log("agentOptInTools checks passed");
}

void main();
