import ProjectContext from "./ProjectContext";
import type { ProjectContext as ProjectContextPayload } from "../../lib/types";

const API_ROWS: [string, string, string, number][] = [
  ["GET", "/api/projects", "folders, prefixes, counts", 1],
  ["GET", "/api/projects/:key/context", "tracked manifest, pinned HEAD documents, verified memory context", 1],
  ["GET", "/api/issues?project=", "parsed frontmatter, derived status, counts", 1],
  ["GET", "/api/issues/:id", "issue body, links both ways, refs, file list, activity", 1],
  ["GET", "/api/issues/:id/file", "raw issue.md, text/markdown", 1],
  ["GET", "/api/issues/:id/activity", "the merged activity stream only", 1],
  ["GET", "/api/agents", "agent list, running and unknown counts", 1],
  ["GET", "/api/health", "pm dir present, daemon reachable, embedded flag", 1],
  ["GET", "/api/stream", "server-sent change events", 3],
  ["POST", "/api/issues", "create an issue folder", 2],
  ["PATCH", "/api/issues/:id", "status, priority, owner, links only", 2],
  ["POST", "/api/issues/:id/comments", "add one comment file", 2],
  ["POST", "/api/issues/:id/artifacts", "upload under the size cap", 2],
  ["GET", "/api/issues/:id/artifacts/:name", "basename only, inside that folder", 2],
];

const ITERS: [string, string, string, string][] = [
  ["I1", "~2 d", "Read-only board", "Issue folders, parser, lint, cadence issue CLI for all writes, cadence ui with the four GET routes, board plus drawer, seed ~/pm, gateway vhost."],
  ["I2", "~1.5 d", "Write path", "Drag to change status, quick-add, comment, attach, edit links. Derived cards refuse manual moves. Constrained artifact read."],
  ["I3", "~1 d", "Agents", "Event stream, busy marker on the active card, fence and parked-delivery alerts with the recovery command."],
  ["I4", "~1 d", "Loops", "A notes chain as a timeline: kickoff, QA, review rounds, verdict, PR and head SHA."],
  ["later", "token", "Dispatch from a card", "First feature that acts rather than views. Waits for auth."],
];

interface PlanProps {
  project: string;
  context: ProjectContextPayload | null;
  contextLoading: boolean;
  contextError: string | null;
  onRetryContext: () => void;
}

export default function Plan({
  project,
  context,
  contextLoading,
  contextError,
  onRetryContext,
}: PlanProps) {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[106rem] w-full">
      <ProjectContext
        project={project}
        context={context}
        loading={contextLoading}
        error={contextError}
        onRetry={onRetryContext}
      />
      <div className="plan-grid grid xl:grid-cols-3 gap-x-8 gap-y-10 mt-10">
        <div className="xl:col-span-2 min-w-0 space-y-16 max-w-[62rem]">
          <section className="reveal">
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Cadence help
              </h2>
              <span className="kicker">implementation reference · not a project plan</span>
            </div>
            <p className="text-body leading-[1.55] max-w-[68ch]">
              This page explains the current Cadence implementation. It is static
              help for the board, not the selected project&apos;s context or plan.
              One folder per
              issue in a private directory outside every repo. The agent-notes
              chain keeps holding evidence. Cadence keeps holding runtime
              state. The board reads all three and writes only issue folders.
            </p>
            <div className="grid sm:grid-cols-3 gap-4 mt-4">
              <div className="card p-5">
                <div className="slabel">intent</div>
                <div className="num text-secondary text-ink-100 mt-2">
                  ~/pm/&lt;project&gt;/&lt;ID&gt;/
                </div>
                <p className="text-secondary text-ink-400 mt-2">
                  Backlog, priority, links, comments, artifacts.
                </p>
              </div>
              <div className="card p-5">
                <div className="slabel">evidence</div>
                <div className="num text-secondary text-ink-100 mt-2">
                  /var/www/agent-notes
                </div>
                <p className="text-secondary text-ink-400 mt-2">
                  Kickoff, QA and verdict notes, joined by loop id.
                </p>
              </div>
              <div className="card p-5">
                <div className="slabel">runtime</div>
                <div className="num text-secondary text-ink-100 mt-2">
                  cadence daemon
                </div>
                <p className="text-secondary text-ink-400 mt-2">
                  Agents, messages, fences and events.
                </p>
              </div>
            </div>
            <p className="text-secondary text-ink-500 mt-3">
              Not goals: multi-host sync, human team workflows, claims or
              heartbeats in the tracker, anything written into a project repo.
            </p>
          </section>

          <section className="reveal" style={{ animationDelay: "60ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Issue folder
              </h2>
              <span className="kicker">
                one folder per issue, path never changes
              </span>
            </div>
            <div className="grid gap-5">
              <pre className="num text-label leading-relaxed rounded-lg border border-ink-700 bg-ink-900 p-4 text-ink-300 !whitespace-pre overflow-x-auto">
{`~/pm/                           `}<span className="text-ink-500">private git repo, never pushed anywhere public</span>{`
├── pm.yaml                     `}<span className="text-ink-500">schema version, statuses, link types, artifact size cap</span>{`
├── README.md                   `}<span className="text-ink-500">the rules, for agents and humans</span>{`
├── .index/                     `}<span className="text-ink-500">generated cache, disposable, gitignored</span>{`
├── cadence/
│   ├── project.yaml            `}<span className="text-ink-500">key, prefix: CAD, repos, components</span>{`
│   ├── CAD-16/
│   │   ├── issue.md            `}<span className="text-ink-500">frontmatter, intent, acceptance checkboxes</span>{`
│   │   ├── comments/
│   │   │   └── 20260917T172400Z-fable-cc.md
│   │   └── artifacts/
│   │       └── scratch-smoke.txt
│   └── CAD-17/issue.md
├── sportslog/project.yaml      `}<span className="text-ink-500">prefix: SPL, components: app api admin web</span>{`
└── ops/project.yaml            `}<span className="text-ink-500">prefix: OPS, work with no repo</span>
              </pre>
              <ul className="grid sm:grid-cols-2 gap-x-8 gap-y-3 text-secondary leading-[1.5]">
                <li><span className="text-ink-100 font-medium">Paths encode nothing that changes.</span> Folder name is the id only. No title slug, no status folder, no nesting under a parent.</li>
                <li><span className="text-ink-100 font-medium">Store only what cannot be derived.</span> Inverse links, readiness, counts, artifact list, activity and sessions are computed.</li>
                <li><span className="text-ink-100 font-medium">Six statuses.</span> <span className="num">backlog ready doing review done dropped</span>. Issues are never deleted, only dropped.</li>
                <li><span className="text-ink-100 font-medium">Links, one direction only.</span> <span className="num">blocked_by parent relates duplicate_of</span>. Only blocked_by affects readiness.</li>
                <li><span className="text-ink-100 font-medium">Sub-issues are full issues.</span> Own id from the same sequence, own folder, a <span className="num">parent</span> field. Two levels only. A parent with children is a container and its status rolls up.</li>
                <li><span className="text-ink-100 font-medium">Sub-issue or checkbox.</span> If it can be assigned, blocked or reviewed separately, it is a sub-issue. Otherwise a Markdown checkbox.</li>
                <li><span className="text-ink-100 font-medium">Artifacts are refs or files.</span> Refs point at PRs, commits, notes, previews. Files live in <span className="num">artifacts/</span> under a 1 MB cap. The listing is the manifest.</li>
                <li><span className="text-ink-100 font-medium">One file per comment.</span> Created exclusively, named by UTC time and author. Append-only, sanitized Markdown, no threading.</li>
                <li><span className="text-ink-100 font-medium">Writes.</span> <span className="num">issue.md</span> edits take a lock and an atomic rename. Comments and artifacts are create-only. Every write is a git commit made by the tool.</li>
                <li><span className="text-ink-100 font-medium">Lint as a pre-commit hook.</span> Schema, dangling and cyclic links, id and folder mismatch, depth, oversize artifacts.</li>
              </ul>
            </div>
          </section>

          <section className="reveal" style={{ animationDelay: "90ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Projects
              </h2>
              <span className="kicker">a named entity, not a cwd</span>
            </div>
            <div className="grid md:grid-cols-2 gap-5">
              <pre className="num text-label leading-relaxed rounded-lg border border-ink-700 bg-ink-900 p-4 text-ink-300 !whitespace-pre overflow-x-auto">
{`# ~/pm/cadence/project.yaml
key: cadence
prefix: CAD
repos:
  - path: ~/Project/cadence
    remote: github.com/favcrm/cadence
components: [adapter, daemon, cli, ui]
default_owner: cookie-cesium`}
              </pre>
              <div className="text-secondary leading-[1.5]">
                <p className="text-ink-100 font-medium mb-2">
                  How the CLI picks a project
                </p>
                <ol className="space-y-1.5 list-decimal pl-5">
                  <li><span className="num">--project</span> or <span className="num">CADENCE_PROJECT</span> wins.</li>
                  <li>The git common dir of the cwd, so any worktree maps to its main checkout, matched by remote URL.</li>
                  <li>Then matched by path.</li>
                  <li>No match fails closed. An agent never files into the wrong board.</li>
                </ol>
                <p className="text-ink-500 mt-3">
                  Worktrees, monorepo sub-apps and an agent launched from
                  another repo all break a one-to-one cwd mapping. Sub-apps
                  are a <span className="num">component</span> label.
                </p>
              </div>
            </div>
          </section>

          <section className="reveal" style={{ animationDelay: "100ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Issues and M3 jobs
              </h2>
              <span className="kicker">intent versus execution</span>
            </div>
            <div className="grid sm:grid-cols-2 gap-4">
              <div className="card p-5">
                <div className="slabel mb-2">issue · files in ~/pm</div>
                <p className="text-secondary text-ink-300 leading-[1.5]">
                  What should happen and why. Backlog, priority, links,
                  discussion, artifacts. Lives with or without the daemon.
                </p>
              </div>
              <div className="card p-5">
                <div className="slabel mb-2">job · daemon tables, M3</div>
                <p className="text-secondary text-ink-300 leading-[1.5]">
                  One execution of a leaf issue: dispatched, running, review,
                  verified. Assignee, worktree, verdict bound to a commit.
                </p>
              </div>
            </div>
            <ul className="mt-4 space-y-2 text-secondary leading-[1.5] max-w-[72ch]">
              <li><span className="text-ink-100 font-medium">One leaf issue, one job, usually one PR.</span> Jobs carry an <span className="num">issue_id</span>. The issue never stores job or session ids.</li>
              <li><span className="text-ink-100 font-medium">Status source.</span> With M3: the job state. Before M3: the latest agent-note whose header carries <span className="num">Issue: CAD-16</span>. Otherwise the file field.</li>
              <li><span className="text-ink-100 font-medium">Sessions are a live view.</span> Agents, provider sessions and messages come from the daemon through the message-to-task column M3 adds.</li>
            </ul>
          </section>

          <section className="reveal" style={{ animationDelay: "120ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Architecture
              </h2>
              <span className="kicker">one binary, three sources</span>
            </div>
            <div className="card p-5 overflow-x-auto">
              <div className="min-w-[620px]">
                <div className="grid grid-cols-[1fr_auto_1fr_auto_1fr] items-stretch gap-3 num text-label">
                  <div className="rounded border border-ink-700 bg-ink-900 p-3">
                    <div className="text-ink-100">browser</div>
                    <div className="text-ink-500 mt-1">Vite · React · Tailwind SPA</div>
                  </div>
                  <div className="self-center text-ink-500">→</div>
                  <div className="rounded border border-ink-700 bg-ink-900 p-3">
                    <div className="text-ink-100">dev gateway nginx</div>
                    <div className="text-ink-500 mt-1">cadence.localhost:18000</div>
                  </div>
                  <div className="self-center text-ink-500">→</div>
                  <div className="rounded border border-ink-600 bg-ink-800 p-3">
                    <div className="text-ink-100">cadence ui · 127.0.0.1</div>
                    <div className="text-ink-500 mt-1">serves dist + /api</div>
                  </div>
                </div>
                <div className="grid grid-cols-3 gap-3 mt-3 num text-label">
                  <div className="rounded border border-dashed border-ink-700 p-3">
                    <span className="text-ink-500">reads and writes</span>
                    <div className="text-ink-100 mt-1">~/pm</div>
                  </div>
                  <div className="rounded border border-dashed border-ink-700 p-3">
                    <span className="text-ink-500">reads only</span>
                    <div className="text-ink-100 mt-1">agent-notes</div>
                  </div>
                  <div className="rounded border border-dashed border-ink-700 p-3">
                    <span className="text-ink-500">reads only</span>
                    <div className="text-ink-100 mt-1">daemon socket</div>
                  </div>
                </div>
              </div>
            </div>
            <ul className="mt-4 space-y-2 text-secondary leading-[1.5] max-w-[72ch]">
              <li><span className="text-ink-100 font-medium">Frontend.</span> <span className="num">cadence/ui/</span>: Vite, React, TypeScript, Tailwind 4, pnpm. Dev uses the Vite proxy to the API.</li>
              <li><span className="text-ink-100 font-medium">Backend.</span> A <span className="num">cadence ui</span> subcommand in the existing Rust binary. It already owns the store and socket. The built SPA is embedded behind a cargo feature so CI without Node still builds.</li>
              <li><span className="text-ink-100 font-medium">Live updates.</span> One server-sent events stream: a file watcher on <span className="num">~/pm</span> and the notes directory, plus the daemon change signal. <span className="text-ink-500">(I3 — I1 reloads by hand.)</span></li>
              <li><span className="text-ink-100 font-medium">CLI parity.</span> <span className="num">cadence issue new | ls --ready | show | set | link | comment | attach | lint</span> uses the same writer, so agents never need HTTP.</li>
            </ul>
          </section>

          <section className="reveal" style={{ animationDelay: "180ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                Design system
              </h2>
              <span className="kicker">shared with agentic-alpha</span>
            </div>
            <p className="text-body leading-[1.55] max-w-[68ch]">
              The UI adopts the sister project&apos;s{" "}
              <span className="num text-secondary">DESIGN.md</span> as written.
              This page is built to it.
            </p>
            <div className="grid sm:grid-cols-2 gap-4 mt-4">
              <div className="card p-5">
                <div className="slabel mb-2">taken as-is</div>
                <ul className="space-y-1.5 text-secondary">
                  <li><span className="num">tokens.css</span> copied verbatim: graphite ink ramp, type scale, field and motion tokens.</li>
                  <li>IBM Plex Sans for prose, Plex Mono with tabular figures for ids, counts and times. No third family.</li>
                  <li>One surface (<span className="num">.card</span>), one border colour, tint chips without borders.</li>
                  <li>Teal accent on interactive elements only.</li>
                  <li>Sidebar plus slim topbar. Detail in an overlay drawer: header, body, sticky footer.</li>
                </ul>
              </div>
              <div className="card p-5">
                <div className="slabel mb-2">mapped for cadence</div>
                <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-2 text-secondary items-center">
                  <span className="chip bg-ok/10 text-ok">ok</span><span>done, healthy, nothing fenced</span>
                  <span className="chip bg-fail/10 text-fail">fail</span><span>fenced, unknown, failed delivery</span>
                  <span className="chip bg-warn/10 text-warn">warn</span><span>blocked, in review, top priority</span>
                  <span className="chip bg-info/10 text-info">info</span><span>derived from notes, agent busy, kickoff notes</span>
                </div>
                <p className="text-label text-ink-500 mt-3">
                  Board is treated as a Registry page: full-width content plus
                  overlay drawer, no persistent rail. Plan is a Document page,
                  so it may carry this context rail.
                </p>
              </div>
            </div>
          </section>

          <section className="reveal" style={{ animationDelay: "240ms" }}>
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="text-section font-semibold text-ink-100">
                API surface
              </h2>
              <span className="kicker">and what it must never grow</span>
            </div>
            <div className="border border-ink-700 bg-ink-875 overflow-x-auto">
              <table className="w-full text-table min-w-[560px]">
                <thead>
                  <tr className="slabel text-left border-b border-ink-700">
                    <th className="px-3.5 py-2.5 font-medium">method</th>
                    <th className="px-3.5 py-2.5 font-medium">path</th>
                    <th className="px-3.5 py-2.5 font-medium">purpose</th>
                    <th className="px-3.5 py-2.5 font-medium text-right">iteration</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-ink-700/80">
                  {API_ROWS.map((r) => (
                    <tr key={r[0] + r[1]} className="hover:bg-ink-850">
                      <td className="px-3.5 py-2.5 num text-ink-400">{r[0]}</td>
                      <td className="px-3.5 py-2.5 num text-ink-100">{r[1]}</td>
                      <td className="px-3.5 py-2.5 text-ink-300">{r[2]}</td>
                      <td className="px-3.5 py-2.5 num text-right text-ink-400">I{r[3]}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="card p-5 mt-4 border-warn/30">
              <div className="flex items-center gap-2 mb-2">
                <span className="chip bg-warn/10 text-warn">
                  no auth at this stage
                </span>
                <span className="kicker">by decision</span>
              </div>
              <p className="text-secondary leading-[1.55] max-w-[76ch]">
                Containment is the defence: bind 127.0.0.1 by default, no CORS,
                reachable only through the gateway vhost and your SSH forward.
                That holds only while the API stays this small.{" "}
                <span className="text-ink-100">Never add</span> arbitrary file
                read, command execution, git or PR actions, or agent dispatch
                without a token. Those endpoints are why the Beads UI needed
                patching.
              </p>
            </div>
          </section>
        </div>

        <aside className="space-y-4 xl:sticky xl:top-[4rem] self-start">
          <div className="card">
            <div className="px-4 py-3 border-b border-ink-700 flex items-baseline gap-2">
              <h3 className="text-cardtitle font-semibold text-ink-100">
                Iterations
              </h3>
              <span className="kicker">one screen each</span>
            </div>
            <ol className="divide-y divide-ink-700/80">
              {ITERS.map((i) => (
                <li key={i[0]} className="px-4 py-3">
                  <div className="flex items-baseline gap-2">
                    <span className="num text-label text-ink-100 w-9">{i[0]}</span>
                    <span className="text-secondary text-ink-100 font-medium">
                      {i[2]}
                    </span>
                    <span className="ml-auto kicker num">{i[1]}</span>
                  </div>
                  <p className="text-label text-ink-400 mt-1 pl-11 leading-[1.5]">
                    {i[3]}
                  </p>
                </li>
              ))}
            </ol>
          </div>
          <div className="card p-4">
            <div className="slabel mb-2">decisions to confirm</div>
            <ul className="space-y-2 text-secondary">
              <li>Backend in the cadence Rust binary, not a Node server. <span className="text-ok">Done.</span></li>
              <li>Tracker directory at <span className="num">~/pm</span>, outside the cadence repo. <span className="text-ok">Done.</span></li>
              <li>Derived status overrides the file field. <span className="text-ok">Done.</span></li>
              <li className="text-ok">Board item is called an issue. Confirmed.</li>
            </ul>
          </div>
          <div className="card p-4">
            <div className="slabel mb-2">guardrails</div>
            <ul className="space-y-2 text-secondary">
              <li><span className="text-ink-100">Stay small.</span> Needing rich queries, several human users or multi-host sync means adopt a real tracker.</li>
              <li><span className="text-ink-100">One write path.</span> Agents write notes and use the CLI for issue folders.</li>
              <li><span className="text-ink-100">The UI is optional.</span> Files and CLI work with the process down.</li>
            </ul>
          </div>
        </aside>
      </div>
    </main>
  );
}
