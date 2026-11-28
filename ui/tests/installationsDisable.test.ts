import type * as WorkspaceApps from "../src/features/workspace-apps/workspaceApps";
declare function require(name: string): any;
export {};
/**
 * CAD-1211: when the session stops being the operator, `useInstallations`
 * is switched off and must drop the operator's last list, error and retry
 * state. Re-enabling loads again.
 */
function equal(a: unknown, e: unknown, why: string) {
  if (JSON.stringify(a) !== JSON.stringify(e)) throw new Error(`${why}: expected ${JSON.stringify(e)}, got ${JSON.stringify(a)}`);
}

async function main() {
  const { Window } = require("happy-dom");
  const win = new Window({ url: "http://localhost/" });
  for (const name of ["window", "document", "Node", "Element", "HTMLElement", "navigator", "location"])
    Object.defineProperty(globalThis, name, { value: name === "window" ? win : win[name], configurable: true, writable: true });
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", { value: true });
  const React = require("react") as typeof import("react");
  const { createRoot } = require("react-dom/client") as typeof import("react-dom/client");
  const { act } = React;
  const { workspaceApps, readInstallationsSnapshot } = require("../src/features/workspace-apps/workspaceApps") as typeof WorkspaceApps;
  const { useInstallations } = require("../src/features/workspace-apps/useInstallations") as typeof import("../src/features/workspace-apps/useInstallations");

  const operatorList = [{ id: "install-notes" }];
  let calls = 0;
  let lastSignal: AbortSignal | undefined;
  (workspaceApps as any).installations = async (signal?: AbortSignal) => {
    calls += 1;
    lastSignal = signal;
    return operatorList;
  };

  let seen: ReturnType<typeof useInstallations> | undefined;
  function Probe({ enabled, paused = false, source = "list" }: { enabled: boolean; paused?: boolean; source?: "list" | "home" }) {
    seen = useInstallations(enabled, "k", source, true, paused);
    return null;
  }
  const root = createRoot(win.document.createElement("div"));
  const settle = () => act(async () => { await new Promise((r) => setTimeout(r, 10)); });

  await act(async () => { root.render(React.createElement(Probe, { enabled: true })); });
  await settle();
  equal(seen?.list, operatorList, "operator list loaded while enabled");
  equal(calls, 1, "one request while enabled");
  await act(async () => { root.render(React.createElement(Probe, { enabled: true, paused: true })); });
  equal(readInstallationsSnapshot(), operatorList, "an authorized paused consumer preserves the shared snapshot");
  equal({ list: seen?.list, error: seen?.error, loading: seen?.loading }, { list: null, error: null, loading: false }, "paused consumer exposes no stale state");
  equal(calls, 1, "pausing does not issue a read");
  await act(async () => { root.render(React.createElement(Probe, { enabled: true })); });
  await settle();
  equal(calls, 2, "resuming reads again");


  await act(async () => { root.render(React.createElement(Probe, { enabled: false })); });
  await settle();
  equal(
    { list: seen?.list, error: seen?.error, retrying: seen?.retrying, loading: seen?.loading },
    { list: null, error: null, retrying: false, loading: false },
    "disabled hook shows nothing of the operator's last state",
  );
  equal(lastSignal?.aborted, true, "the enabled request is aborted on disable");
  equal(readInstallationsSnapshot(), null, "loss of authority still evicts the shared snapshot");

  await act(async () => { root.render(React.createElement(Probe, { enabled: true })); });
  await settle();
  equal(calls, 3, "re-enabling requests again");
  equal(seen?.list, operatorList, "re-enabled hook loads the list again");

  // Mid-flight pause/session reset must refuse without writing or returning stale installations.
  const { readInstallationsGuarded, dropInstallationsSnapshot } =
    require("../src/features/workspace-apps/workspaceApps") as typeof WorkspaceApps;
  const { pausePrivateReads, allowPrivateReads } =
    require("../src/lib/privateSession") as typeof import("../src/lib/privateSession");
  dropInstallationsSnapshot();
  let resolveInFlight!: (rows: any) => void;
  (workspaceApps as any).installations = () => new Promise((resolve) => { resolveInFlight = resolve; });
  const pending = readInstallationsGuarded();
  pausePrivateReads();
  resolveInFlight(operatorList);
  let refused = false;
  try {
    await pending;
  } catch (error: any) {
    refused = error?.status === 401;
  } finally {
    allowPrivateReads();
  }
  equal(refused, true, "mid-flight pause refuses installations result with 401");
  equal(readInstallationsSnapshot(), null, "refused mid-flight read never caches under current session");

  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input) => {
    const url = new URL(String(input), win.location.href);
    if (url.pathname !== "/api/app-home") throw new Error(`Unexpected member read: ${url.pathname}`);
    const installations = ["ok", "off", "removed"].map((state) => ({
      install_id: `install_${state}`, name: state, title: state,
      attention: { state, message: null, action: null, count: 0 },
    }));
    return new Response(JSON.stringify({ installations }), { headers: { "Content-Type": "application/json" } });
  };
  try {
    await act(async () => { root.render(React.createElement(Probe, { enabled: true, source: "home" })); });
    await settle();
    const { canOpenInstall } = require("../src/features/explorer/installAccess") as typeof import("../src/features/explorer/installAccess");
    const launchable = seen?.list?.filter((row) => canOpenInstall(row.state, row.removed)).map((row) => row.install_id);
    equal(launchable, ["install_ok"], "member launch links exclude disabled and removed installs from app home");
  } finally {
    globalThis.fetch = originalFetch;
  }

  await act(async () => { root.unmount(); });
  win.close?.();
}
void main().then(() => console.log("installations disable passed"));
