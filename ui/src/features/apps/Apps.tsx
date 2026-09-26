import { resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { AppRow } from "../../lib/types";
import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import ApproveApp from "./ApproveApp";
import { appApprovalChip, appHref, sourceLabel, unboundSlots } from "./apps";
import type { Viewer } from "../projects/work";

/**
 * The Apps screen (CAD-557): one card per installed app per project,
 * straight from the daemon's `app ls` read — name, version, approval
 * state, the declared connection slots with their bindings (unbound
 * flagged), the bundle's workflows, and the git source with its pinned
 * SHA when installed from git. A card opens `/apps/<project>/<name>`.
 *
 * The sidebar's project scope filters the list; the empty state names
 * the install command, scoped to the selected project when there is one.
 */
export default function Apps({ project, viewer }: { project: string; viewer: Viewer }) {
  const state = useQuery(resources.apps);
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
          <AppCard key={`${row.project}/${row.name ?? i}`} row={row} viewer={viewer} />
        ))}
      </ul>
    </main>
  );
}

function AppCard({ row, viewer }: { row: AppRow; viewer: Viewer }) {
  const approval = appApprovalChip(row);
  const unbound = new Set(unboundSlots(row));
  const source = sourceLabel(row);
  const href = row.name ? appHref(row.project, row.name) : null;
  return (
    <li className="card px-3.5 py-3 min-w-0" data-app={row.name ?? undefined}>
      <div className="flex items-start gap-2 min-w-0">
        {href ? (
          <Link href={href} className="min-w-0 break-words hover:text-accent">
            <span className="num text-label text-accent">{row.project}/{row.name}</span>
            <span className="text-cardtitle font-medium text-ink-100 ml-2">
              {row.title ?? row.name}
            </span>
          </Link>
        ) : (
          <span className="min-w-0 break-words">
            <span className="num text-label text-ink-400">{row.project}/{row.name ?? "?"}</span>
          </span>
        )}
        <span className={`chip shrink-0 ${approval.cls}`}>{approval.text}</span>
      </div>
      <div className="flex flex-wrap items-center gap-1.5 mt-2">
        {row.version && <span className="chip bg-ink-800 text-ink-300">v{row.version}</span>}
        {(row.workflows ?? []).map((wf) => (
          <span key={wf} className="chip bg-ink-800 text-ink-300 num">
            {row.name}/{wf}
          </span>
        ))}
        {(row.connections ?? []).map((c) => (
          <span
            key={c.slot}
            className={`chip num ${c.bound == null ? "bg-warn/10 text-warn" : "bg-ink-800 text-ink-300"}`}
            title={c.bound == null ? `slot ${c.slot} is unbound` : `slot ${c.slot} → ${c.bound}`}
          >
            {c.slot} → {c.bound ?? "unbound"}
          </span>
        ))}
        {unbound.size > 0 && (
          <span className="chip bg-warn/10 text-warn">
            {unbound.size} unbound slot{unbound.size === 1 ? "" : "s"}
          </span>
        )}
      </div>
      {row.error && (
        <p className="text-label text-fail mt-2 break-words" role="note">
          {row.error}
        </p>
      )}
      {source && <p className="num text-micro text-ink-500 mt-2 break-all">{source}</p>}
      <ApproveApp row={row} viewer={viewer} />
    </li>
  );
}
