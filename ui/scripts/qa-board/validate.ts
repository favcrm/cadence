/**
 * CAD-2291: the QA board's fixture payloads, checked with the UI's own client
 * code — the parsers and loaders the board UI runs on these routes — so a
 * payload the UI would refuse stops `qa-board up` (and fails the ui test)
 * instead of being served as a drifted fake.
 *
 * `qa-board up` compiles this file against the served checkout's `ui/src`
 * with the ui tests' compiler options, then runs it as
 * `node validate.js <fixtures-dir> <viewer-url>`.
 */
import { kickoffBlock, writeBlock } from "../../src/features/auth/gate";
import { mutatePersonalProfile, readPersonalBootstrap, readPersonalIdentity } from "../../src/features/assistant/identity";
import { parseBrowserRead, readBrowser, stopBrowser, viewBrowser } from "../../src/features/canvas/browserClient";
import { memberHistory, memberThreadStore, sendToAssistant } from "../../src/features/home/memberThread";
import { parseMount } from "../../src/features/workspace-apps/screen/screenLifecycle";
import { screenProjection, screenTag } from "../../src/features/workspace-apps/screen/screenProjection";
import { shapeFor } from "../../src/features/workspace-apps/screen/screenProtocol";
import type { AppContext, AppEffect, HomeInstallation, Installation, WorkspaceRun } from "../../src/features/workspace-apps/workspaceApps";
import { api } from "../../src/lib/api";
import { allowPrivateReads } from "../../src/lib/privateSession";
import type { Meta } from "../../src/lib/types";

declare const require: { (name: string): any; main: unknown };
declare const module: unknown;
declare const process: { argv: string[]; exit(code: number): never; stdout: { write(text: string): void } };

/** Fixture file name (without `.json`) → payload. */
export type Fixtures = Record<string, unknown>;

/** The browser session presets the control endpoint can load. */
export const BROWSER_PRESETS = ["handoff", "handoff-signed-in", "active", "ended"] as const;

const CONVERSATION = "qa-board-conversation";

export function loadFixtures(dir: string): Fixtures {
  const fs = require("fs");
  const path = require("path");
  const out: Fixtures = {};
  for (const name of fs.readdirSync(dir).sort()) {
    if (name.endsWith(".json")) out[name.slice(0, -5)] = JSON.parse(fs.readFileSync(path.join(dir, name), "utf8"));
  }
  return out;
}

/** A browser body the UI accepts as a session, or why not. The control
 *  endpoint uses this too, so a state set mid-run is checked the same way. */
export function checkBrowser(body: unknown): string | null {
  const read = parseBrowserRead(body);
  if (read === null) return "parseBrowserRead refused it";
  if (read.kind !== "session") return "parseBrowserRead read it as no browser";
  const raw = body as Record<string, unknown>;
  // `""` reads back as null: a fixture must say what it means.
  if (read.session.state !== raw.state || read.session.signedIn !== (raw.signedIn === true)
    || read.session.url !== (raw.url ?? null) || read.session.instructions !== (raw.instructions ?? null)) {
    return "parseBrowserRead did not read it back unchanged";
  }
  return null;
}

/** Answers the board routes from `fx`, as the QA board's proxy does. */
function fixtureFetch(fx: Fixtures, browser: () => unknown, viewUrl: string): typeof fetch {
  return (async (input: unknown, init?: { method?: string; body?: unknown }) => {
    const url = new URL(String(input), "http://qa-board.invalid");
    const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
    const request = typeof init?.body === "string" ? JSON.parse(init.body) as Record<string, unknown> : {};
    switch (`${init?.method ?? "GET"} ${url.pathname}`) {
      case "GET /api/browser": return json(browser());
      case "POST /api/browser/view": return json({ url: viewUrl });
      case "POST /api/browser/stop": return json(fx["browser-stop"]);
      case "POST /api/member-assistant/state": return json(fx["member-assistant-state"]);
      case "POST /api/member-assistant/history": return json(fx["member-assistant-history"]);
      case "POST /api/member-assistant/profile": return json(fx["member-assistant-profile"]);
      case "POST /api/member-assistant/send": return json({ ...(fx["member-assistant-send"] as object), message: request.message });
      default: return new Response("{}", { status: 404 });
    }
  }) as typeof fetch;
}

const failed = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** Every failure, each naming its payload; empty when the UI accepts them all. */
export async function validateFixtures(fx: Fixtures, viewUrl: string): Promise<string[]> {
  const failures: string[] = [];
  const fail = (payload: string, why: string) => failures.push(`${payload}.json: ${why}`);
  for (const name of ["meta", "locale", "browser-stop", "member-assistant-state", "member-assistant-history", "member-assistant-profile", "member-assistant-send", ...BROWSER_PRESETS.map((p) => `browser-${p}`),
    "app-home", "app-installation", "app-contexts", "app-runs", "app-effects", "app-screen-mount"]) {
    if (!(name in fx)) fail(name, "missing");
  }
  if (failures.length > 0) return failures;

  // `/api/meta`: the sign-in admission itself is inline in AuthBoundary (not
  // importable); these are the UI's exported readers of the same answer.
  const meta = fx.meta as Meta;
  if (writeBlock(meta) !== null) fail("meta", `writeBlock refused it: ${writeBlock(meta)}`);
  if (kickoffBlock(meta) === null) fail("meta", "kickoffBlock reads it as the operator, not a member");

  // The member app screen, as the board's WorkspaceApp builds it: the screen
  // tag, the projection and its v2 push (as ui/scripts/app-fixtures/producer.ts
  // does), and the mount receipt.
  const installation = fx["app-installation"] as Installation;
  const tag = screenTag(installation);
  if (tag === null) fail("app-installation", "screenTag finds no mountable screen");
  else {
    const contexts = (fx["app-contexts"] as { contexts: AppContext[] }).contexts;
    const runs = (fx["app-runs"] as { runs: WorkspaceRun[] }).runs;
    const effects = (fx["app-effects"] as { effects: AppEffect[] }).effects;
    const contextId = contexts[0]?.id ?? "";
    try {
      const projection = screenProjection(installation, tag, contextId, contexts, runs, effects,
        { installId: installation.install_id, contextId, status: "ok", intents: [] },
        { installId: installation.install_id, contextId, bindings: [], workers: null, texts: new Map() });
      if (!shapeFor(projection, "v2")) fail("app-runs", "shapeFor refused the screen's v2 push");
    } catch (error) { fail("app-contexts", `screenProjection refused it: ${failed(error)}`); }
    if (parseMount(fx["app-screen-mount"], tag) === null) fail("app-screen-mount", "parseMount refused it");
  }
  const home = (fx["app-home"] as { installations?: HomeInstallation[] }).installations ?? [];
  if (!home.some((row) => row.install_id === installation.install_id && row.name === installation.name)) {
    fail("app-home", "no row names app-installation.json, so /apps/<key> would not resolve");
  }

  for (const preset of BROWSER_PRESETS) {
    const why = checkBrowser(fx[`browser-${preset}`]);
    if (why) fail(`browser-${preset}`, why);
  }

  const original = globalThis.fetch;
  let browser: unknown = fx["browser-handoff"];
  globalThis.fetch = fixtureFetch(fx, () => browser, viewUrl);
  allowPrivateReads();
  try {
    for (const preset of BROWSER_PRESETS) {
      browser = fx[`browser-${preset}`];
      try {
        const read = await readBrowser(CONVERSATION);
        if (read.kind !== "session") fail(`browser-${preset}`, "readBrowser read no browser");
      } catch (error) { fail(`browser-${preset}`, `readBrowser refused it: ${failed(error)}`); }
    }
    try {
      const view = await viewBrowser(CONVERSATION);
      if (view.kind !== "url" || view.url !== viewUrl) fail("browser-view", "viewBrowser did not return the viewer URL");
    } catch (error) { fail("browser-view", `viewBrowser refused ${viewUrl}: ${failed(error)}`); }
    try { await stopBrowser(CONVERSATION); } catch (error) { fail("browser-stop", `stopBrowser refused it: ${failed(error)}`); }

    try {
      const state = await api.memberAssistState();
      if (readPersonalIdentity(state).state !== "ready") fail("member-assistant-state", "readPersonalIdentity read no identity");
      if (readPersonalBootstrap(state) === "pending") fail("member-assistant-state", "bootstrap is pending: member chat would open on first-time setup");
      if (typeof state.conversation_id !== "string" || state.conversation_id === "") fail("member-assistant-state", "no conversation: the Browser card would never load");
    } catch (error) { fail("member-assistant-state", `memberAssistState refused it: ${failed(error)}`); }

    const outcome = await mutatePersonalProfile((body) => api.memberAssistProfileUpdate(body), { expected_revision: 1, action: "update", name: "QA" });
    if (!outcome.ok) fail("member-assistant-profile", `mutatePersonalProfile refused it: ${outcome.error}`);

    const store = memberThreadStore("qa-board:validate");
    await store.refresh();
    const history = memberHistory(store.get());
    if (!history.loaded || history.error) fail("member-assistant-history", `memberThreadStore refused it: ${history.error?.text ?? "not loaded"}`);
    else if ((store.get().data?.entries.length ?? 0) === 0) fail("member-assistant-history", "no entries: member chat would show an empty thread");

    const sent = await sendToAssistant("qa-board:validate", "hello", "qa-board-check");
    if (!sent.ok) fail("member-assistant-send", `sendToAssistant refused it: ${sent.error}`);
  } finally {
    globalThis.fetch = original;
  }
  return failures;
}

if (typeof require === "function" && require.main === module) {
  const [dir, viewUrl] = process.argv.slice(2);
  validateFixtures(loadFixtures(dir), viewUrl).then((failures) => {
    process.stdout.write(`${JSON.stringify({ ok: failures.length === 0, failures })}\n`);
    process.exit(failures.length === 0 ? 0 : 1);
  }, (error: unknown) => {
    process.stdout.write(`${JSON.stringify({ ok: false, failures: [`validator crashed: ${failed(error)}`] })}\n`);
    process.exit(1);
  });
}
