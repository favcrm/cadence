export {};
/**
 * CAD-2726: Day-0 starter prompts appear on empty workspaces and revert to
 * operational prompts once projects or agents exist.
 */
import { streamFixture } from "./streamFixture";
import { assertCondition as assert, installHostedDom, waitForDom } from "./hostedDom.fixture";
import type * as ReactModule from "react";
import type * as ReactDOM from "react-dom/client";
import type * as HomeModule from "../src/features/home/Home";
import type * as Locale from "../src/lib/locale";
import type * as Stores from "../src/lib/resources";

declare function require<T>(name: string): T;

installHostedDom("https://acme.cadencecloud.app/", { stubMarkdown: true });

const React = require<typeof ReactModule>("react");
const { createRoot } = require<typeof ReactDOM>("react-dom/client");
const Home = require<typeof HomeModule>("../src/features/home/Home").default;
const { LocaleProvider } = require<typeof Locale>("../src/lib/locale");
const { resources } = require<typeof Stores>("../src/lib/resources");

const identity = { name: null, tone: null, look: "robot", revision: "fixture" };
const masterPayload = {
  alias: "master",
  identity,
  provider: "fixture",
  endpoint_kind: "fixture",
  live: true,
  turn: null,
  queue: [],
};
const json = (value: unknown) =>
  new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });

globalThis.fetch = async (input, init) => {
  const url = new URL(String(input), "https://acme.cadencecloud.app");
  if (url.pathname.endsWith("/stream")) return streamFixture(url.href, masterPayload as never, init?.signal);
  if (url.pathname === "/api/threads/master") {
    return json({ thread: { id: "home-thread", alias: "master" }, entries: [], more_before: false });
  }
  if (url.pathname === "/api/master/state") return json(masterPayload);
  if (url.pathname === "/api/agents") {
    return json({ daemon: "reachable", agents: [{ alias: "master", state: "running" }], profiles: [], totals: null });
  }
  if (url.pathname === "/api/meta") return json({ operator: true, read_only: false });
  return json({});
};

async function main(): Promise<void> {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);

  await React.act(async () => {
    resources.agents.write(() => ({
      daemon: "reachable",
      agents: [{ alias: "master", state: "running" }],
      profiles: [],
      totals: null,
    }) as never);
  });

  // 1. workspaceEmpty = true -> Day-0 prompts
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Home, {
          readOnly: false,
          hosted: true,
          overview: resources.overview.get(),
          onOpenIssue: () => undefined,
          onOpenDecision: () => undefined,
          permissionsHref: "/settings/permissions",
          resolveAppPath: () => null,
          workspaceEmpty: true,
        }),
      ),
    );
  });
  await waitForDom(
    React.act,
    host,
    () =>
      (host.querySelector('[data-empty="thread"]')?.textContent ?? "").includes(
        "Help me set up my first project and break down the initial milestones.",
      ),
    "Day-0 prompts on empty workspace",
  );

  const day0Text = host.querySelector('[data-empty="thread"]')?.textContent ?? "";
  assert(
    !day0Text.includes("What is every agent working on right now, and is anything stuck?"),
    "omits active-team prompt when workspace is empty",
  );

  // 2. workspaceEmpty = false -> standard operational prompts
  await React.act(async () => {
    root.render(
      React.createElement(
        LocaleProvider,
        null,
        React.createElement(Home, {
          readOnly: false,
          hosted: true,
          overview: resources.overview.get(),
          onOpenIssue: () => undefined,
          onOpenDecision: () => undefined,
          permissionsHref: "/settings/permissions",
          resolveAppPath: () => null,
          workspaceEmpty: false,
        }),
      ),
    );
  });
  await waitForDom(
    React.act,
    host,
    () =>
      (host.querySelector('[data-empty="thread"]')?.textContent ?? "").includes(
        "What is every agent working on right now, and is anything stuck?",
      ),
    "operational prompts on populated workspace",
  );

  await React.act(async () => root.unmount());
  host.remove();
  console.log("homeDay0Prompts checks passed");
}

declare const process: { exit: (code?: number) => void };
main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
