/** CAD-2477: byte-identical copy of the AgenticOS v2 managed Wiki fixture. */
import { getManagedFile } from "../src/features/wiki/managedFiles";
import { locationHref, readLocation } from "../src/lib/router";

import type { DocumentFollowUps } from "../src/lib/types";

declare function require(name: string): { readFileSync(path: string, encoding: "utf8"): string };
declare const process: { cwd(): string; exitCode?: number };
const { readFileSync } = require("fs");
const fixture = JSON.parse(
  readFileSync(`${process.cwd()}/../tests/fixtures/agenticos/wiki-managed-files.contract.json`, "utf8"),
) as { member_overview: { document_follow_ups: DocumentFollowUps }; managed_detail: unknown };

async function run() {
  const { document_follow_ups: followUps } = fixture.member_overview;
  if (followUps.status !== "ready" || followUps.pending.length !== 1) throw new Error("Missing member review fixture");
  const pending = followUps.pending[0];
  const target = new URL(pending.href, "https://board.example.test");
  const location = readLocation(target.pathname, target.search);
  const { route } = location;
  if (route.screen !== "wiki" || route.file !== pending.id || route.version !== 2 || locationHref(location) !== pending.href) {
    throw new Error("Home review did not resolve to the submitted Wiki version");
  }
  const original = globalThis.fetch;
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: { getItem: () => null, setItem: () => {} },
  });
  globalThis.fetch = async () => Response.json(fixture.managed_detail);
  try {
    const detail = await getManagedFile(route.file, route.version);
    if (detail.document?.submittedVersion !== 2 || !detail.actions.approve) throw new Error("Assigned review action was lost");
    const refused = await getManagedFile(route.file, 1).then(() => false, () => true);
    if (!refused) throw new Error("A version-1 Evidence link rendered the version-2 response instead");
  } finally {
    globalThis.fetch = original;
    Reflect.deleteProperty(globalThis, "localStorage");
  }
  console.log("Managed Wiki contract: exact Home version and refusal of a mismatched response");
}

void run().catch((error) => { console.error(error); process.exitCode = 1; });
