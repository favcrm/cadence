import {
  readAppUrlState,
  readStoredProjectView,
  writeStoredProjectView,
} from "../src/lib/urlState";

function equal(actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

const legacy = readAppUrlState("?project=cadence&view=list", "kanban");
equal(legacy.tab, "board");
equal(legacy.view, "list");
equal(legacy.project, "cadence");

const projectOnly = readAppUrlState("?project=cadence", "list");
equal(projectOnly.tab, "overview");
equal(projectOnly.view, "list");

const explicitKanban = readAppUrlState("?tab=board&view=kanban", "list");
equal(explicitKanban.tab, "board");
equal(explicitKanban.view, "kanban");

// Serializing moved to the router — tests/router.test.ts (locationHref).

const settings = readAppUrlState("?tab=settings&project=cadence", "kanban");
equal(settings.tab, "settings");
equal(settings.project, "cadence");

let stored: string | null = null;
const storage = {
  getItem: () => stored,
  setItem: (_key: string, value: string) => {
    stored = value;
  },
};
equal(readStoredProjectView(storage), undefined);
writeStoredProjectView("list", storage);
equal(readStoredProjectView(storage), "list");
equal(readStoredProjectView({ getItem: () => { throw new Error("blocked"); } }), undefined);

console.log("urlState checks passed");
