import {
  FORBIDDEN_CONTENT_KEYS,
  contentClient,
  contentPaths,
} from "../src/features/app-shell/contentClient";
import { ApiError } from "../src/lib/api";

export {};
/** CAD-782: content paths bind the URL scope and bodies stay grammatical. */

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}
function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

async function main() {
  const scope = { installId: "install-a", contextId: "ctx-b" };
  equal(
    contentPaths.savePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns",
    "save path binds the URL scope",
  );
  equal(
    contentPaths.showPath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1",
    "show path binds install, context and campaign",
  );
  equal(
    contentPaths.renderPath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/render",
    "render path binds the campaign",
  );
  equal(
    contentPaths.approvePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/approve",
    "approve path binds the campaign",
  );
  equal(
    contentPaths.testPreparePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/test-prepare",
    "test-prepare path binds the campaign",
  );
  equal(
    contentPaths.sendPreparePath(scope, "launch-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/campaigns/launch-1/send-prepare",
    "send-prepare path binds the campaign",
  );
  equal(
    contentPaths.proposePath(scope),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals",
    "propose path binds the URL scope",
  );
  equal(
    contentPaths.proposalApplyPath(scope, "prop-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals/prop-1/apply",
    "apply path binds the proposal",
  );
  equal(
    contentPaths.proposalDiscardPath(scope, "prop-1"),
    "/api/app-installations/install-a/contexts/ctx-b/content/proposals/prop-1/discard",
    "discard path binds the proposal",
  );
  // No identity, routing or discovery-link key may travel from the browser.
  for (const key of [
    "by",
    "actor",
    "workspace",
    "project",
    "project_link",
    "install_id",
    "context_id",
  ]) {
    assert(
      (FORBIDDEN_CONTENT_KEYS as readonly string[]).includes(key),
      `content client forgot forbidden key ${key}`,
    );
  }
  // The client mirrors the peer grammar: save/propose/apply bodies
  // refuse unknown fields before a byte is sent.
  let refused = 0;
  const realFetch = (globalThis as any).fetch;
  (globalThis as any).fetch = () =>
    Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
  try {
    // Monkey-patch the body guard through a forged call shape: the
    // client's assertClean throws ApiError 400 client-side.
    for (const call of [
      () =>
        (contentClient as any).save(scope, {
          campaignId: "launch-1",
          subject: "S",
          blocks: [],
          by: "operator",
        }),
    ]) {
      try {
        await call();
      } catch (error) {
        assert(
          error instanceof ApiError && (error as ApiError).status === 400,
          "forged content body did not refuse client-side",
        );
        refused += 1;
      }
    }
    assert(refused === 1, "forged content body reached the network");
  } finally {
    (globalThis as any).fetch = realFetch;
  }
}

// The tests directory runs each compiled file directly; mirror the
// sibling suites' shape.
void main();
