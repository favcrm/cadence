import {
  readAppUrlState,
  readStoredProjectView,
  serializeAppUrlState,
  writeStoredProjectView,
} from "./urlState";
import { NO_FILTERS } from "./filters";

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

const serialized = new URLSearchParams(
  serializeAppUrlState("?unknown=keep&tag=stale&view=list", {
    tab: "board",
    view: "kanban",
    project: "cadence",
    openId: null,
    filters: NO_FILTERS,
  }),
);
equal(serialized.get("unknown"), "keep");
equal(serialized.get("tab"), "board");
equal(serialized.get("view"), "kanban");
equal(serialized.get("project"), "cadence");
equal(serialized.has("tag"), false);

const overviewUrl = new URLSearchParams(
  serializeAppUrlState("?project=cadence&view=list", {
    tab: "overview",
    view: "list",
    project: "cadence",
    openId: null,
    filters: NO_FILTERS,
  }),
);
equal(overviewUrl.get("project"), "cadence");
equal(overviewUrl.has("tab"), false);
equal(overviewUrl.has("view"), false);

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
