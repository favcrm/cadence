import { useEffect, useRef, useState } from "react";
import { resources } from "../../lib/resources";
import { useMaybeResource, useQuery, useResource } from "../../lib/useResource";
import { useHref } from "../../lib/useLocation";
import Link from "../../ui/Link";
import Button from "../../ui/Button";
import SectionTabs from "../../ui/SectionTabs";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { homeNeeds } from "../home/needs";
import HowTab from "./HowTab";
import PostsTab from "./PostsTab";
import RunDrawer from "./RunDrawer";
import SettingsTab from "./SettingsTab";
import {
  appApprovalChip,
  appHref,
  appNeeds,
  appPurpose,
  isReadyToRun,
  notReadyText,
  primaryAction,
} from "./appViewModel";
import type { Viewer } from "../projects/work";
import "./apps.css";

type Tab = "posts" | "settings" | "how";
type AppDetailProps = {
  project: string;
  name: string;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onHome: () => void;
};

/**
 * `/apps/<project>/<name>` (CAD-563 r2) — one installed app, built
 * around what a person does with it: what needs them (the strip), a new
 * run (the drawer), the runs in flight and what has been published
 * (Posts), the team and where it publishes (Settings), and how it works
 * (How it works, with the engine's details folded away). `?new=<wf>`
 * opens the New-run drawer from a workflow link. Section links keep the
 * selected view in the URL for refresh and browser navigation.
 */
export default function AppDetail(props: AppDetailProps) {
  // Drafts, pending UI and the run drawer belong to one installation.
  return <AppPage key={`${props.project}/${props.name}`} {...props} />;
}

function AppPage({
  project,
  name,
  viewer,
  onOpenIssue,
  onHome,
}: AppDetailProps) {
  const key = `${project}/${name}`;
  const state = useQuery(resources.app(key));
  const app = state.data;
  const [running, setRunning] = useState<string | null>(null);
  // The runs and the Needs-you rail's rows: the Posts tab and the strip
  // read both. The overview is asked for by App.tsx on this screen.
  const runsState = useQuery(resources.appRuns(key));
  const runs = runsState.data ?? [];
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  // The ledger is the operator's: fetch it only when the board proves
  // the operator, so a viewer without the read never sees a 403.
  const outputsRes = viewer.operator ? resources.appOutputs(key) : null;
  const outputs = useMaybeResource(outputsRes);
  useEffect(() => {
    if (outputsRes) void outputsRes.revalidate();
  }, [outputsRes]);
  const items = outputs?.data?.items ?? [];
  const pending = outputs?.data?.pending ?? [];
  // `?new=<app>/<wf>` asks for that workflow's drawer once the app has
  // loaded; consumed once, so closing it does not reopen on a refetch.
  const href = useHref();
  const params = new URLSearchParams(href.split("?")[1] ?? "");
  const selectedTab = params.get("tab");
  const tab: Tab = selectedTab === "settings" || selectedTab === "how" ? selectedTab : "posts";
  const appPath = appHref(project, name);
  const settingsHref = `${appPath}?tab=settings`;
  const want = params.get("new");
  const consumed = useRef<string | null>(null);
  useEffect(() => {
    if (want && app && consumed.current !== want) {
      consumed.current = want;
      setRunning(want);
    }
  }, [want, app]);

  const action = app ? primaryAction(app) : null;
  const approval = app ? appApprovalChip(app) : null;
  // Installation approval lives in Settings; this strip is for run decisions.
  const needsRows = app ? appNeeds(app, runs, needs, pending).filter(n => n.kind !== "approve") : [];
  const ready = app ? isReadyToRun(app) : false;
  const notReady = app ? notReadyText(app) : null;
  return (
    <>
      <main className="apps-workspace px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label={`app ${key}`}>
        <Link href="/apps" className="lnk text-label">
          ← All apps
        </Link>
        <div className="flex flex-wrap items-start gap-3 mt-2 mb-3">
          <div className="min-w-0 flex-1">
            <h1 className="text-section font-semibold text-ink-100">
              {app?.title?.trim() || name}
            </h1>
            <p className="text-micro text-ink-500 mt-0.5">Project: {project}</p>
            {app && <p className="text-body text-ink-400 mt-1 break-words">{appPurpose(app)}</p>}
            <div className="flex flex-wrap items-center gap-2 mt-1.5">
              {approval && app?.approval !== "approved" && (
                <span className={`chip ${approval.cls}`}>{approval.text}</span>
              )}
              <StaleChip state={state} />
            </div>
          </div>
          {action && (
            <Button
              variant="primary"
              onClick={() => ready && setRunning(action.wf.name)}
              disabled={!ready}
              className="shrink-0"
            >
              {action.label}
            </Button>
          )}
        </div>
        {app && !ready && (
          <section
            className="app-setup card px-4 py-3 min-w-0 mb-3"
            aria-label="setup required"
          >
            <div className="min-w-0">
              <h2 className="text-label font-medium text-ink-100">Setup required</h2>
              <p className="text-label text-ink-400 mt-0.5 break-words">{notReady}</p>
            </div>
            {tab !== "settings" && <Button href={settingsHref}>Open settings</Button>}
          </section>
        )}
        <ResourceGate
          state={state}
          loading={`loading ${key}…`}
          failed={`could not load ${key}`}
          onRetry={() => void resources.app(key).invalidate()}
        />

        {app && needsRows.length > 0 && (
          <section
            className="card px-4 py-3 min-w-0 mb-3 border-warn/40"
            aria-label="needs you"
          >
            <div className="slabel mb-1.5">needs you</div>
            <ul className="space-y-1.5">
              {needsRows.map((n, i) => (
                <li key={`${n.kind}-${i}`} className="flex flex-wrap items-center gap-2 min-w-0">
                  <span className="text-label text-ink-200 min-w-0 break-words flex-1">{n.text}</span>
                  <Link href="/" className="lnk text-label shrink-0" title="open Needs you">
                    {n.kind === "release" ? "Release →" : "Answer →"}
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        )}

        {app && (
          <>
            <div className="-mx-4 lg:-mx-8 mb-4">
              <SectionTabs label="App" tabs={[
                {label:"Posts",href:appPath,on:tab === "posts"},
                {label:"Settings",href:settingsHref,on:tab === "settings"},
                {label:"How it works",href:`${appPath}?tab=how`,on:tab === "how"},
              ]} />
            </div>
            {tab === "posts" && (
              <PostsTab
                runs={runs}
                runsState={runsState}
                needs={needs}
                items={items}
                pending={pending}
                ready={ready}
                viewer={viewer}
                onOpenIssue={onOpenIssue}
                onRetry={() => void resources.appRuns(key).invalidate()}
              />
            )}
            {tab === "settings" && (
              <SettingsTab
                app={app}
                runs={runs}
                viewer={viewer}
                onApproveRetry={() => void resources.app(key).invalidate()}
              />
            )}
            {tab === "how" && <HowTab app={app} runs={runs} />}
          </>
        )}
      </main>
      {running && app && (
        <RunDrawer
          project={project}
          app={app}
          wf={running}
          runs={runs}
          viewer={viewer}
          onClose={() => setRunning(null)}
          onOpenIssue={(id) => {
            setRunning(null);
            onOpenIssue(id);
          }}
          onHome={onHome}
        />
      )}
    </>
  );
}
