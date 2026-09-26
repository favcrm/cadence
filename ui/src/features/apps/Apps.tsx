import { resources } from "../../lib/resources";
import { useQuery, useResource } from "../../lib/useResource";
import type { AppRow } from "../../lib/types";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { homeNeeds, type HomeNeed } from "../home/needs";
import {
  appApprovalChip,
  appHref,
  appPurpose,
  newRunHref,
  runsSummary,
} from "./apps";
import type { Viewer } from "../projects/work";

/**
 * The Apps screen (CAD-563 r2): one card per installed app — a
 * monogram, the title, the app's one-line purpose, what is happening
 * now ("2 in progress · 1 needs you", read from the app's runs) and the
 * primary action that starts a new run ("New post"). Internals — the
 * slug, the version, the path, the digest, the slot bindings — live on
 * the app page, not here.
 *
 * The in-page project filter (the chip row above this list) narrows
 * the cards; the empty state names the install command, scoped to the
 * selected project when there is one.
 */
export default function Apps({ project }: { project: string; viewer: Viewer }) {
  const state = useQuery(resources.apps);
  // The Needs-you rail's own rows, when the board has read the overview
  // (the app page asks for it; the list never fetches it itself).
  const overview = useResource(resources.overview);
  const needs = homeNeeds(overview.data?.needs_me);
  const all = state.data ?? [];
  const rows = project === "all" ? all : all.filter((r) => r.project === project);
  return (
    <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="apps">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h1 className="text-section font-semibold text-ink-100">Apps</h1>
        <StaleChip state={state} />
        <span className="kicker">
          installed apps{project === "all" ? "" : ` · ${project}`}
        </span>
      </div>
      <ResourceGate
        state={state}
        loading="loading apps…"
        failed="could not load apps"
        onRetry={() => void resources.apps.invalidate()}
      />
      {state.data && rows.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          No apps installed{project === "all" ? "" : ` in ${project}`} —{" "}
          <code className="num text-ink-300">
            cadence app install &lt;path|git-url&gt; --project {project === "all" ? "<key>" : project}
          </code>{" "}
          puts one here.
        </div>
      )}
      <ul className="space-y-2.5">
        {rows.map((row, i) => (
          <AppCard key={`${row.project}/${row.name ?? i}`} row={row} needs={needs} />
        ))}
      </ul>
    </main>
  );
}

/** A monogram tile — the app's first letter, on the board's accent. */
function AppIcon({ label }: { label: string }) {
  const letter = (label.trim()[0] ?? "A").toUpperCase();
  return (
    <span
      aria-hidden
      className="shrink-0 w-9 h-9 rounded-lg bg-accent/15 text-accent grid place-items-center text-cardtitle font-semibold"
    >
      {letter}
    </span>
  );
}

function AppCard({
  row,
  needs,
}: {
  row: AppRow;
  needs: HomeNeed[];
}) {
  const approval = appApprovalChip(row);
  const title = row.title?.trim() || row.name || "App";
  const appName = row.name;
  const href = appName ? appHref(row.project, appName) : null;
  const action = row.primary ?? null;
  return (
    <li className="card px-3.5 py-3 min-w-0" data-app={row.name ?? undefined}>
      <div className="flex items-start gap-3 min-w-0">
        <AppIcon label={title} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2 min-w-0">
            {href ? (
              <Link href={href} className="min-w-0 break-words hover:text-accent">
                <span className="text-cardtitle font-medium text-ink-100">{title}</span>
              </Link>
            ) : (
              <span className="text-cardtitle font-medium text-ink-100">{title}</span>
            )}
            <span className={`chip shrink-0 ${approval.cls}`}>{approval.text}</span>
          </div>
          <p className="text-label text-ink-400 mt-0.5 break-words">{appPurpose(row)}</p>
          <p className="text-micro text-ink-500 mt-1" data-summary>
            {row.error ? (
              row.error
            ) : appName ? (
              <AppActivity project={row.project} name={appName} needs={needs} />
            ) : (
              "activity unavailable"
            )}
          </p>
        </div>
        {appName && href && action && (
          <Link
            href={newRunHref(row.project, appName, `${appName}/${action.workflow}`)}
            className="shrink-0 h-8 px-3 rounded bg-accent text-on-accent text-label font-medium grid place-items-center"
          >
            {action.label?.trim() || "New run"}
          </Link>
        )}
        {!action && href && (
          <Link
            href={href}
            className="shrink-0 h-8 px-3 rounded border border-ink-600 text-label text-ink-300 grid place-items-center hover:border-edge-hover"
          >
            Open app
          </Link>
        )}
      </div>
    </li>
  );
}

/**
 * The card's live line: the app's runs — the same shared store the app
 * page uses. Its own component so the runs hook is called
 * unconditionally, for the rows that have an app to read (CAD-571 N2).
 */
function AppActivity({
  project,
  name,
  needs,
}: {
  project: string;
  name: string;
  needs: HomeNeed[];
}) {
  const runs = useQuery(resources.appRuns(`${project}/${name}`));
  if (runs.status === "failed") return <>activity unavailable</>;
  if (!runs.data) return <>reading activity…</>;
  return <>{runsSummary(runs.data, needs) ?? "Nothing running yet"}</>;
}
