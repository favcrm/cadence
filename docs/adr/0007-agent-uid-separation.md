# 0007 — Agents under a dedicated uid without sudo

- Status: **proposed**. This document changes no code and no host state.
  It is the design the operator accepts or amends before CAD-461's
  implementation tickets are filed.
- Risk class **human** (trigger 1 — a trust boundary: who the operator
  and the agents are) for every implementation ticket it spawns. Each
  needs an independent adversarial security review before it merges.
- Date: proposed 2026-09-25.
- Author: `swe-461` (devin worker), dispatched by `op-pm`.
- Deciders: the operator — §11 lists the questions this ADR asks.
- Issues: CAD-461 (this ADR). It is the follow-up ADR 0005 scheduled at
  its acceptance (§12 Q1, §13 T8), the live-authority half of its
  residual R7, and ADR 0004's "supported hardened deployment" (option
  D). It also bears on ADR 0006 §1.5 P4 / §5.3: the custody store's
  same-uid residual closes when the store's readers are a different uid.
  PR #275 (CAD-366) carries that store — open at this writing; custody
  was verified daemon-owned `0700` under the state dir at its head —
  and this ADR is written so the store needs no change under
  separation; it only becomes private for real.
- **Code citations are pinned to
  `ea62ec96f66d67c7e5c8974e7d9de137c78e9fd7`** (origin/main, 2026-09-25).
  Line numbers move; re-locate by the quoted symbol.

## 1. Context

### 1.1 Why now

Every cadence agent on this host — tmux panes, managed provider
endpoints, the master — runs as `ubuntu` (uid 1000), the operator's own
uid, which holds NOPASSWD sudo and docker/lxd membership. ADR 0005
measured the consequence (its §1.4): no file the operator owns is closed
to an agent, no operator process is unsignalable, and every "operator
proof" is a heuristic about process shape that any same-uid caller can
reproduce. The operator accepted that posture as a guardrail and
scheduled Option D — this ADR — as the boundary (ADR 0005 §12 Q1).

The immediate trigger was the CAD-366 security review on PR #275: a
reviewer planted a platform token in the daemon's custody store and read
it back with `cat` from an ordinary agent pane. Custody is a promise
that no *offered surface* carries credential bytes; on one uid the store
itself is one `cat` away (ADR 0006 §1.5 P4, §5.3). The operator decided
to ship custody with an honest same-uid warning and make uid separation
the real fix. This is that fix's design.

### 1.2 What runs as the operator's uid today

An inventory at the pinned commit. "Ownership" below means both the
filesystem owner and the uid every mechanism assumes.

| Surface | What it is | Where |
|---|---|---|
| The daemon | One process, owns the state dir, binds `<state>/cadence.sock` | `src/daemon.rs` `serve`, `acquire_singleton` |
| Socket admission | `SO_PEERCRED`; the peer **must have the daemon's euid** or it is refused before dispatch | `src/daemon.rs` `check_peer` |
| Pty agents | `tmux -L cadence-<statehash> new-session -d -c <worktree> -e CADENCE_ALIAS=… <command>` on a private tmux server; panes are the server's children, so the server's uid | `src/adapter/pty/mod.rs` `PtyAdapter::open`, `tmux_socket`, `pane_env` |
| Managed endpoints | Provider CLIs spawned as daemon children over stdio or a loopback websocket | `src/adapter/mod.rs` `build` (`endpoint_kind "managed"`), `src/adapter/stdio.rs` `StdioAdapter`, `src/adapter/ws.rs`, `src/adapter/claude.rs`, `src/adapter/codex.rs` |
| Caller identity | The nearest registered pane or enrolled endpoint on the peer's `/proc` ancestry; the operator is whoever `operator_proof` admits | `src/daemon/caller_rule.rs` `RULES`, `src/peer.rs` `operator_proof`, `src/slots/strict.rs` `Enrollment` |
| Board writes over TCP | The peer's client socket is found in `/proc/net/tcp` and its owner pid resolved through `/proc/<pid>/fd` — the fd walk is same-uid only, but the socket's uid column is readable cross-uid (`Attribution::Foreign`, `decide`) | `src/peer.rs` `client_socket`, `socket_owners`, `tcp_peer_agent`; `src/ui/operator.rs` `attribute`, `decide` |
| Worktrees | `git worktree add <repo>/.cadence/wt/<name>` from whoever runs `issue start`; objects live in the main checkout's `.git` | `src/worktree.rs` `create_worktree`, `src/worktree/layout.rs` |
| Shared dep cache | `<repo>/.cadence/target/shared` — one artifact store for every lane | `src/worktree.rs` `shared_target_dir`, `SHARED_DEBUG_DIRS` |
| Tracker | `~/pm` (or `CADENCE_PM_DIR`) — a git checkout every agent writes directly (Class 4 writes, ADR 0005 §1.2) | `src/issue/write.rs` `actor_who`; `CADENCE_PM_DIR` reaches panes via `pane_env`'s context (`src/master.rs`) |
| Briefings | `<state>/briefings/<root>/BRIEFING-<alias>.md`, read by the agent from disk | `src/client.rs` `briefing_path`, `src/main.rs` `refresh_briefing` |
| Operator material | `<state>/operator/secret`, `<state>/operator/sessions.json`, the signers file ADR 0005 adds; plus `cadence.sqlite3`, `slots.json`, daemon/ui logs | `src/operator_auth.rs` `dir`, `secret_path`, `Auth::load`; `src/daemon.rs` |
| The binary | `~/.local/bin/cadence` → `~/.local/share/cadence/releases/<sha>/cadence`; `install.sh --prefix` exists | `src/upgrade.rs`; `docs/SESSION.md`; `scripts/install.sh` |
| Provider logins | `~/.claude` + `~/.claude.json`, `~/.codex`, `~/.config/devin` + `~/.local/share/devin`, `~/.config/cursor` + `~/.cursor`, `~/.pi`, `~/.config/gh`, `~/.ssh`, `~/.gitconfig`, `~/.npmrc` | `src/setup.rs` `PROVIDERS` sign-in signals |
| Build toolchains | `~/.cargo` (`rustc-wrapper = ~/.cargo/bin/sccache`), `~/.rustup`, `~/.local/share/pnpm`; a live sccache server on `127.0.0.1:4226` | `~/.cargo/config.toml`, `ss -ltn` |
| Cleanup paths | Pane kill, session reap (SIGTERM → bounded wait → SIGKILL of survivors), managed-endpoint `child.kill()` | `src/adapter/pty/mod.rs` `kill_pane`, `src/adapter/pty/lane.rs` `reap_session`, `src/adapter/stdio.rs` |

### 1.3 Host facts that bound this design (measured 2026-09-25)

| Fact | How it was measured |
|---|---|
| Agents run as `ubuntu` uid 1000; groups `ubuntu adm cdrom sudo dip lxd docker` | `id` in this lane's pane |
| `ubuntu` has `(ALL) NOPASSWD: ALL`, and `docker`/`lxd` membership — already root-equivalent | `sudo -n -l`, `getent group` |
| No `cadence*` or `agent*` user or group exists | `getent passwd`, `getent group` |
| `/home/ubuntu` is `drwxr-x---` — a foreign uid cannot even traverse it; `~/Project` and the repo are `0775` | `ls -ld` |
| tmux 3.4; per-uid socket dirs `/tmp/tmux-<uid>/` are `0700`; this host's holds many `cadence-<hash>` sockets | `ls /tmp/tmux-1000`, `tmux -V` |
| `/proc` mounted without hidepid: `stat`, `status`, `cmdline`, `net/tcp` are world-readable; `environ`, `fd/`, `cwd`, `fdinfo` need ptrace-level access (cross-uid **denied**, `kernel.yama.ptrace_scope = 1`) | `mount`, `src/peer.rs` read sites (§1.2) |
| `runuser`, `setpriv`, `systemd-run`, `setfacl` present; no `newuidmap`; `apparmor_restrict_unprivileged_userns = 1` (no unprivileged-userns helpers) | `command -v`, `/proc/sys` |
| systemd 255; `ubuntu` has `Linger=yes`; a user manager runs for uid 1000 | `systemctl --version`, `loginctl show-user` |
| Board listens `127.0.0.1:3010`; sccache listens `127.0.0.1:4226` as `ubuntu` | `ss -ltnp` |
| The state dir `~/.local/state/cadence` is `0700`; contains `cadence.sock`, `cadence.sqlite3`, `slots.json`, `operator/secret` (0600), `briefings/`, `agents/*.provider.log`, logs | `ls -la` (read-only) |
| 280 GB free on `/` | `df -h` |

### 1.4 What separation must not break

- The daemon keeps working when some agents are still uid-1000 and some
  are the agent uid (mixed fleet through the rollout).
- The operator still attaches to panes, reads screens, sends keys
  (`cadence attach`, `agent capture`, the stall watch's `sample_screen`).
- `SO_PEERCRED`, `/proc/<pid>/stat` ancestry, pid-starttime and
  enrollment verification must keep working for agent-uid processes;
  anything that depended on cross-`/proc` reads that now fail is named
  explicitly rather than silently degraded (§6, §9).
- ADR 0005's grants are unchanged: uid is a *narrowing* fact, never a
  grant. uid==daemon-uid remains necessary-but-not-sufficient for the
  operator; uid==agent-uid is sufficient proof of *not*-operator.
- Test and CI posture: no second uid exists in CI. The separation is a
  deployment mode with a default-off config; the harness keeps running
  same-uid (§10).

## 2. Decision 1 — the uid model

**Options.**

- **U1 — one `cadence-agent` uid for every agent and lane.** One system
  user (locked password, no login shell requirement — panes exec a
  command directly), one home `~cadence-agent`, one primary group
  `cadence-agent`, plus membership in one shared group `cadence` that
  the operator also joins. No sudoers entry; no other groups.
- **U2 — one uid per lane** (`cadence-a`, `cadence-b`, … or a
  pre-provisioned pool `cadence-agent01..N`). Real isolation between
  lanes: peer worktrees, peer provider sessions and peer processes are
  all cross-uid. Costs: `useradd` needs root *per lane* (lanes are
  created constantly — either a standing pool with a reconciliation
  chore, or a root-on-lane-start dependency), N credential sets (one
  provider login per uid, or a shared credential house — see §7's
  honest problem), and all of §4–§8's sharing machinery duplicated.
- **U3 — one uid per provider** (`cadence-claude`, `cadence-devin`, …).
  Isolates provider credential stores per uid but not lanes — two
  Claude lanes still share everything. Perverse middle: most of U2's
  cost for a fraction of its isolation.

**Recommendation: U1** for stage 1, with U2 named as the tightening
path. Rationale: every intra-agent attack U2 prevents is already
possible today between same-uid agents, so U1 buys the entire
operator↔agent boundary at a single uid's cost; U2 can be a later
ticket keyed on a per-lane uid pool, and nothing in U1's shape blocks
it (the launch helper takes the target uid as configuration, §5). U2's
real blocker is honest: a lane is created in milliseconds today, and a
design that needs `useradd` mid-flow or a pool with free-list
accounting is a different ticket's worth of machinery.

Group layout under U1:

| Group | Members | Purpose |
|---|---|---|
| `cadence-agent` | `cadence-agent` only (primary) | Default file group for agent-owned data |
| `cadence` | `ubuntu`, `cadence-agent` | The shared edge: daemon socket dir, tracker, worktree trees, shared dep cache. Nothing agent-private and nothing operator-private joins it. |

The agent account: `useradd --system --shell /usr/sbin/nologin`, locked
password, **no sudoers line**, no supplementary groups beyond `cadence`.
"Without sudo" is load-bearing, not a detail: ADR 0005 option D's own
verdict is that a sudo-capable agent uid makes the whole exercise
worthless.

## 3. Decision 2 — what the daemon keeps private

Three custody zones. The rule that generates the table: **the agent uid
can reach only paths an operator has deliberately published into a
shared or agent zone.**

| Zone | Paths | Owner:group, mode | Why |
|---|---|---|---|
| Operator-private (agent uid cannot reach at all) | `~/.local/state/cadence/**` — `cadence.sqlite3`, `slots.json`, `operator/`, custody store (PR #275), `daemon.log`, `ui.log`, `sessions/`, `private/`, `reviews/`, `GATE-STATE.md`; `~/.ssh`, `~/.config/gh`, `~/.claude*`, `~/.codex`, `~/.config/devin`, `~/.local/share/devin`, `~/.cursor*`, `~/.pi`, `~/.npmrc`, `~/.cargo`, `~/.rustup`, `~/.gitconfig` | `ubuntu:ubuntu`, dir `0700`, files as today | Everything that is an operator proof input or a credential stays behind uid. `/home/ubuntu` is `0750` and **never receives an ACL or traversal grant** — §4's store lives outside `~` precisely so this stays true. |
| Shared edge (`/var/lib/cadence`, root-created once) | `daemon.sock` — the agent-reachable control socket; `pm/` — the relocated tracker checkout; `lanes/` — optional per-lane drop area | `/var/lib/cadence` `ubuntu:cadence` `0750`; socket `0660`; `pm/` `cadence-agent:cadence` `2775` (setgid) | Agents must reach the daemon socket and the tracker; the directory's lack of group `w` means agents can traverse and connect but cannot create, rename or unlink the socket (no squatting). `pm/` is group-writable by design — Class-4 direct tracker writes stay direct writes — and is agent-*owned* on purpose: its `.git` falls under §4's invariant (git there only ever runs as the agent uid). |
| Agent-owned | `~cadence-agent/**` — provider config, `gh` bot creds, `.gitconfig`, cargo/rustup/npm, `/tmp` scratch; plus `/var/lib/cadence/repos/<name>/**` — each repo's agent-facing clone, its lane worktrees and its shared dep cache | `cadence-agent:cadence-agent` `0750` home; `repos/` `cadence-agent:cadence` `2750` + default ACL `g:cadence:rwX` | A real home is what makes provider CLIs, toolchains and `gh` work unmodified. `repos/` is agent-owned for a load-bearing reason — git's dubious-ownership refusal is the mechanism that keeps operator-uid git out of it (§4). |

Moves this implies, each one deliberate:

1. **The daemon socket gets a second, shared path.** `client::socket_path`
   is `<state>/cadence.sock` today. The daemon binds *both* the legacy
   private socket and `/var/lib/cadence/cadence.sock` (`0660
   ubuntu:cadence`). The legacy bind keeps every operator-side caller,
   test and doc working; the shared bind is the one agent panes are
   pointed at: pane env sets a new `CADENCE_SOCKET` (consulted before
   the state-dir join) and drops `CADENCE_STATE_DIR` entirely, so a
   file-read assumption fails fast instead of reaching the operator's
   state dir. Dual-bind is permanent — it costs a second listener.
2. **The tracker moves out of `$HOME`.** `~/pm` →
   `/var/lib/cadence/pm`, `mv` at relocation, `~/pm` becomes
   an operator-side symlink (agents can't resolve it — `/home/ubuntu`
   traversal fails first, which is fine, they use `CADENCE_PM_DIR`).
   The moved checkout is `chown`ed to `cadence-agent` — its `.git`
   joins §4's invariant: git inside it only ever runs as the agent
   uid, whoever initiates the write. **The move, the `chown` and the
   symlink land in this ADR's T6 (§13), in the same change as the
   shared-git wrapper:** chowning earlier would make git's
   dubious-ownership refusal fire on every still-uid-1000 `cadence
   issue` write — the seatbelt working, but before the wrapper exists
   to carry those writes. Trust semantics unchanged: today any
   same-uid process can write the tracker; after the move any
   agent-uid process can. ADR 0005's T6 (tracker writes via the
   daemon) already plans to route tracker writes through the daemon or
   stamp them unproven; this ADR does not wait on it.
3. **Briefings move into the lane.** `client::briefing_path` writes
   `<state>/briefings/<root>/BRIEFING-<alias>.md`, which agents read
   from disk — unreachable after separation. The launch path writes it
   to `<worktree>/.cadence/<group>/BRIEFING-<alias>.md` instead — the
   same path AGENTS.md already names, now real inside the lane — and
   `--cwd` lanes without a worktree get
   `/var/lib/cadence/lanes/<alias>/`. Either way the "read your
   briefing" bootstrap survives verbatim.
4. **The binary moves to a shared prefix.** `install.sh --prefix
   /opt/cadence` already exists (`docs/SESSION.md` documents it).
   Releases land `root`- or `ubuntu`-owned under `/opt/cadence`
   (`0755`); agents execute, never write. The operator's
   `~/.local/bin/cadence` stays the operator's own entry point. Pane
   `PATH` is rebuilt by the launcher — inherited ubuntu paths like
   `~/.local/bin` would fail `EACCES` anyway; better to ship the agent a
   clean PATH than an unreachable one.
5. **`CADENCE_SUITE_LOCK` and `/tmp` roots.** The suite lock already
   honours an env var; the launcher sets it into
   `/var/lib/cadence/lanes/` or per-lane under the worktree — never the
   private default.
6. **Managed repos get an agent-facing store, not a hole in `~`.**
   `/home/ubuntu` is never ACL'd: agents work in clones under
   `/var/lib/cadence/repos/<name>` and their worktrees inside them.
   §4 is this decision — including why the traverse-ACL alternative
   is rejected.

What agents may read/write (explicitly): their worktree inside the
store, the store itself, their lane drop, the tracker, the shared dep
cache (§8), the daemon socket, their own home, generic `/tmp`.
Nothing else is granted.

## 4. Decision 3 — worktrees, repositories and forge auth

**The traversal problem first.** `/home/ubuntu` is `0750`: an agent-uid
process cannot stat *anything* under the operator's home, so the
existing layout `~/Project/<repo>/.cadence/wt/<lane>` is unreachable
before any file permission matters. The tempting one-line fix — a
traversal ACL on the home — is a trap, and it is measured, not
hypothetical (metadata only; nothing was read): `~/.claude.json` plus
four tmp/backup copies are `664`, `~/.gitconfig` `664`,
`~/.google-ads.yaml` and `~/.meta-ads.yaml` `664`; `~/.aws` is `775`
with `cli/cache/session.db` `644`; `~/.password-store` is `775` with
every `.gpg` ciphertext `664`; `~/.claude`, `~/.codex`, `~/.cursor`,
`~/.pi`, `~/.cargo`, `~/.rustup` are all `775`, leaving provider
session transcripts and MCP configs world-readable
(`~/.cursor/mcp.json` `664`); and `~/Project` itself is `0775` — one
traversal grant opens every sibling repo, `.env` files at `644`/`664`
in a dozen projects included, among them
`~/Project/sit/api/gcloud-credentials.json` `644`, a GCP
service-account key. `x` on `/home/ubuntu` converts all of it into
agent-readable material — the custody boundary this ADR exists to
build would be moot while sibling secrets leak.

**Options.**

- **R1 — traverse-only ACLs + a lockdown sweep.** `setfacl -m
  u:cadence-agent:x` on `/home/ubuntu` and `/home/ubuntu/Project`, then
  provision recursively strips `o` outside the published edge and
  `doctor` asserts no agent-reachable `o+r` path under `~` survives.
  Workable — but the sweep is a *standing* obligation, not a one-time
  fix: every future `0644` dotfile, extracted archive or installer
  re-opens the leak until the next sweep, and a routine `chmod 700 ~`
  silently recuts the ACL anyway. Available for an operator who
  refuses to move anything (§11 Q7); not recommended on a
  mixed-content home.
- **R2 — an agent-owned store outside `~`.** The agent-facing
  checkouts live at `/var/lib/cadence/repos/<name>` — provision clones
  the operator's checkout once via `git clone file://…` (never
  `--local`/`--shared`, so no object inode is ever shared with the
  operator's `.git`), `chown`s it to `cadence-agent`, and repoints
  `origin` at the forge, which is the only remote it ever syncs with.
  Lane worktrees are `repos/<name>/.cadence/wt/<lane>` off the store's
  `.git` — `create_worktree` already takes a root; nothing forced it
  to be the operator's checkout. Nothing under `~` is shared at all:
  no ACL, no traversal grant, `0750` stays the boundary. And the
  operator's `~/Project/<name>` never moves — no `worktree repair`,
  no IDE churn.
- **R3 — worktrees only, out of home.** Keep the main checkout
  private; `git worktree add` pointed outward leaves `gitdir:`
  back-pointers into `<repo>/.git/worktrees/*` that the agent must
  write — the same `.git` exposure R2 removes outright, so it buys
  nothing. Rejected.

**Recommendation: R2** — and a second finding decides its shape:

**A `.git` an agent can write must only ever execute under the agent
uid.** The textbook multi-user-repo recipe — `core.sharedRepository
group`, `chgrp -R cadence .git`, setgid dirs, a default ACL — assumes
a *trusted* group; here the group is adversarial. Group-write on
`.git` is group-write on `config`, `hooks/`, `include.path` targets,
`core.fsmonitor`, `core.sshCommand`, filter drivers — and write on the
`.git/` directory alone suffices (rename `config`, drop a hook file).
The operator's next routine `git` in that checkout executes
agent-controlled code as uid 1000: an agent→operator code-exec
channel that defeats the boundary this ADR builds. Nor can the
sensitive files be carved out — `index.lock`, `packed-refs.lock` and
auto-`gc` all need top-level `.git` write, so git offers no
partial-trust sharing of a common dir. The same applies to the
tracker's `.git` — `pm/` is a shared checkout too. So the rule is
topological, not configurational:

1. **Every `.git` an agent can write is wholly agent-owned.** The
   repo store and the tracker checkout belong to `cadence-agent` —
   config, hooks, objects, refs. Agents may rewrite them freely; it
   buys nothing, per rule 2. Agent *ownership* — not mere
   writability — is load-bearing: git's `safe.directory`
   dubious-ownership refusal is what mechanically stops a
   foreign-uid git from operating there.
2. **Git in agent-reachable checkouts only ever runs as the agent
   uid.** Agent panes run git natively; daemon- and operator-initiated
   git against `/var/lib/cadence` — `issue start`'s `create_worktree`,
   tracker commits, store fetches and prunes — execs through the
   helper as the agent uid: one spawn wrapper, not per-callsite
   judgement. The operator deliberately sets no `safe.directory`
   exception for `/var/lib/cadence`, so a planted hook or config can
   only ever run as the agent uid — there is no residual code-exec
   channel to name. Ad-hoc operator archaeology clones the store into
   `~` (copying *into* operator space is safe); it never runs git
   inside it.
3. **The two object stores never meet.** The store's `origin` is the
   forge; the operator's checkout uses the same forge. They never
   fetch from each other, so a forged local ref or poisoned object in
   the agent store reaches the operator's `.git` only through the
   same reviewed-push path as any branch.

**Who creates the worktree.** The `issue start` caller — usually the
PM, itself agent-uid after cutover. `create_worktree`'s root is
per-lane-mode: `repos/<name>` for agent-uid lanes, the operator's
checkout for the uid-1000 lanes the transition still has (the
project's declared-repo list names both). A uid-1000 caller's git
against the store execs through the helper per rule 2; an agent-uid
PM can no longer create uid-1000 lanes at all, which is the direction
of travel anyway. `issue start` fetches `origin` in the store first,
so lanes branch from current main. Commit identity:
`~cadence-agent/.gitconfig` sets `user.name`/`user.email` to a shared
`cadence-agent` identity — commits are already attributable by
`Issue:` trailer and branch; a per-pane `GIT_AUTHOR_NAME=<alias>` via
`pane_env` is an optional refinement, not required.

**`gh` and push auth.** The hard requirement from the brief: agents
push branches and open PRs but never see the operator's own tokens.
Options:

- **F1 — a dedicated bot credential for the agent uid.** A fine-grained
  PAT on a machine account, or a GitHub App installation token, scoped
  to `favcrm/cadence` (contents + pull requests, nothing else), stored
  `0600` at `~cadence-agent/.config/gh/hosts.yml`; `gh auth setup-git`
  once makes git-over-https use it too — the store's `origin` fetches
  and the lanes' pushes alike. All PRs carry the bot author —
  attribution shifts to the `Issue:` trailer and the `cadence/<slug>`
  branch, which is already the convention.
- **F2 — a deploy key.** One ssh keypair in `~cadence-agent/.ssh` with
  write scope on the repo. `gh` still needs *some* token for PR
  operations, so this replaces git transport but not `gh`; two
  credentials where F1 needs one.
- **F3 — credential brokering.** The daemon holds the forge token and
  exposes `gh`-as-an-RPC. The honest version of the CAD-366 pattern —
  and a much bigger build; the custody epic (ADR 0006) is where that
  belongs, not here.

**Recommendation: F1**, scoped to the one repo this host's fleet works
on. Residual stated plainly: every agent-uid process can use the bot
credential — an agent can push to or open PRs from *any* lane's branch.
That is today's exact posture (the shared `gh` login), moved off the
operator's account — strictly better blast radius. `~/.ssh` is not
copied; the agent uid gets no ssh keys unless a later ticket needs one.

## 5. Decision 4 — the launch path without runtime sudo

What must run as the agent uid: every pty pane's command, and every
managed endpoint's provider process (`StdioAdapter`, `WsAdapter`). What
must also cross uid: signals for `agent stop`/reap, and two `/proc`
reads that fail cross-uid (`environ`, `cwd`).

**Options.**

- **L1 — a setuid-root drop helper, installed once.** A small binary
  `/opt/cadence/libexec/cadence-agent-exec` (`root:cadence-launch`
  `4750`; the group contains only `ubuntu`) with three verbs:
  `exec [--env K=V …] -- <argv>` (setgid to the agent's primary group,
  `setgroups` to exactly `{agent-primary, cadence}` — no inherited
  operator groups — setuid, env allowlist, `execvp`); `kill <pid>
  <sig>` (drops uid, then signals — the kernel confines it to
  agent-uid targets for free); `inspect <pid>` (drops uid, prints the
  pid's `/proc` cwd/stat facts the daemon can no longer read). The
  daemon wraps pane commands and provider spawns in it. Synchronous,
  stateless, ~150 lines — it parses nothing at runtime, resolves the
  target uid from a root-owned config file (never argv or env),
  bounds `kill` to `pid>0` and a fixed signal allowlist, and closes
  fds >2 before exec — with identical spawn semantics to today (the
  daemon stays the parent; `wait()` works; `kill()` goes through the
  helper).
- **L2 — a systemd service supervisor.** `cadence agentd serve` under
  `User=cadence-agent`, owning `/var/lib/cadence/agentd.sock` (`0660`,
  group `cadence`), serving a small RPC: spawn (with `SCM_RIGHTS` fd
  passing for the stdio pipes), signal, proc-inspect, reap. No setuid
  file exists at all; the audit surface is in-repo Rust. The honest
  costs: fd-passing and orphan semantics (supervisor children outlive a
  daemon restart; restart wipes, re-adoption policy) are a few hundred
  lines and a new failure mode — *supervisor down* becomes "cannot
  launch" where the helper is stateless.
- **L3 — `sudo -n -u cadence-agent` everywhere.** Works *today* —
  `ubuntu` already has NOPASSWD ALL. Zero install. But it is runtime
  sudo — the thing this ticket removes as a dependency — and it
  disappears if the operator ever tightens `ubuntu`'s sudoers. It is
  the right *transitional* mechanism for the observe phase (§10 uses
  exactly this before the helper exists), not the final answer.
- **L4 — a login session: `ssh localhost` / `machinectl shell`.**
  Real separation, wrong tool: needs sshd config, key custody and a
  session lifecycle of its own; the daemon's per-call spawns become
  round-trips through a login shell. Rejected.
- **L5 — `systemd-run` / polkit.** `systemd-run --system -p
  User=cadence-agent` needs the `manage-units` polkit action, which is
  not narrow — it authorizes creating *any* system unit as *any* user.
  Rejected on grant width, not mechanism.

**Recommendation: L1**, with L2 as the growth path if the helper ever
needs more than its three verbs (e.g. per-lane scratch provisioning or
an agent-uid reaper). The deciding trade, stated honestly: the helper's
*intended* crossings never reach root — it drops to `cadence-agent`
before it execs or signals, so a *correct* helper only ever crosses
`ubuntu` → `cadence-agent`. But a *defect* in a setuid-root binary —
argv parsing, env handling, fd hygiene — runs attacker-controlled code
as **root** before the drop happens; that is the canonical setuid
lesson, and accepting L1 means accepting exactly that artifact. It is
why the helper stays ~150 lines, parses nothing at runtime, resolves
the target uid from a fixed source, bounds `kill` to `pid>0` and a
signal allowlist, closes fds >2, and gets T2's adversarial list. On
*this* host the calculus is softened by fact, not principle: the
caller already has NOPASSWD sudo, so the helper adds no privilege
path *here* — an argument that must not be generalised to "never →
root" on other hosts. L2 removes the setuid artifact entirely and is
the honest alternative if the operator reads setuid-root as a hard
no — that is §11 Q1.

What needs root **once**, at provision (the whole list):

```text
useradd --system -m -d /home/cadence-agent -s /usr/sbin/nologin cadence-agent
groupadd cadence && usermod -aG cadence ubuntu && usermod -aG cadence cadence-agent
groupadd cadence-launch && usermod -aG cadence-launch ubuntu     # helper's caller scope
install -o root -g cadence-launch -m 4750 cadence-agent-exec /opt/cadence/libexec/
install -d -o ubuntu        -g cadence -m 0750 /var/lib/cadence
install -d -o ubuntu        -g cadence -m 0750 /var/lib/cadence/lanes
install -d -o cadence-agent -g cadence -m 2750 /var/lib/cadence/repos
setfacl -m d:g:cadence:rwX /var/lib/cadence/repos
install -d -o ubuntu -g cadence -m 0755 /opt/cadence/bin /opt/cadence/releases
<per repo>: git clone file:///home/ubuntu/Project/<name> /var/lib/cadence/repos/<name> \
            && chown -R cadence-agent:cadence /var/lib/cadence/repos/<name> \
            && git -C /var/lib/cadence/repos/<name> remote set-url origin <forge-url>
```

All of it is one `cadence agent-uid provision` script the operator runs
with sudo once, or by hand from this section. Nothing above runs at
daemon runtime, and nothing touches `/home/ubuntu` — that absence is
the point, and `doctor` asserts it. Note the `install -d` lines are
deliberately one directory each: a single `install -d` with several
`-o/-g/-m` sets applies the last set to every target, which would land
`pm/` at `0750` instead of `2775`.

One transition exposure `usermod -aG cadence-launch ubuntu` carries:
group membership is per-*username*, so until stage D finishes every
still-uid-1000 agent pane can also exec the helper — joining the
agent-uid domain to signal agent-uid peers and read their `/proc`
environ (§12's Bearer-token residual). It gains no operator authority
a uid-1000 process did not already have, and the exposure ends when
nothing runs as uid 1000 any more — named, not excused.

## 6. Decision 5 — tmux and sockets

**Keep the cadence tmux server on uid 1000.** This is the decision that
surprises, so it is argued, not assumed:

- The tmux server's uid is its panes' uid *only if* the pane command
  doesn't drop. With L1 the pane command is `cadence-agent-exec exec --
  <argv>`: the tmux-spawned shell exists as uid 1000 for the microseconds
  before the helper's exec (it runs no user code — the command string is
  fixed), and everything the operator interacts with — every provider,
  every child, every subsequent `respawn-pane` — is agent-uid.
- `/tmp/tmux-1000` is `0700` — after separation an agent-uid process
  cannot reach *any* tmux server on it: not the cadence socket
  (`CAD-288` closes for real, not by narrowing), not the operator's own
  `aos-pm`, not a server it started itself (any server it starts lands
  in `/tmp/tmux-<agentuid>` — reachable, but it holds only that agent's
  own processes; nothing cadence-managed lives there).
- Operator attach, `send-keys`, `capture-pane`, `list-clients`, the
  stall watch — all unchanged. They are daemon-side operations against
  a same-uid server, exactly as today.
- Pane destruction still reaps via the kernel: closing the pty master
  delivers SIGHUP to the pane's session regardless of uid; detached
  survivors are what `lane::reap_session` exists for, and its signals
  route through `agent-exec kill` for agent-uid pids (same-uid kill
  rules apply inside the helper).
- The alternative — an agent-uid tmux server on a group-shared socket —
  inverts the good property: every agent can then drive *every* lane's
  pane (`new-window`, `send-keys`, `capture-pane`). Today that lateral
  reach exists because everything is uid 1000; the recommendation
  *removes* it rather than re-creating it under a new uid.

**The daemon socket** is §3's dual-bind: `/var/lib/cadence/cadence.sock`
`0660 ubuntu:cadence` beside the legacy `<state>/cadence.sock`.
`check_peer` (`daemon.rs`) changes from "peer uid == euid" to an admit
set `{euid, configured agent_uid}` — every other uid refused before
dispatch, as today. The peer's uid is captured at accept and carried
with the pid into caller derivation (§9).

**Cross-uid `/proc` casualties, named:**

| Read | Used by | Cross-uid | Fix |
|---|---|---|---|
| `stat` (ppid, sid, starttime) | ancestry, `Enrollment`, `proc_starttime`, `pane_pid` checks | **works** (world-readable, no hidepid) | none |
| `status` (Uid line) | `peer.rs` uid reads | works | none |
| `environ` (`CADENCE_ALIAS`, `CADENCE_RUNNER_ID`) | `peer.rs` agent-shape checks on a peer's ancestry | **fails** | not needed: uid already answers "agent side" (§9); for operator-side peers it is same-uid and still works |
| `fd/` readlink (pane-pty tie, TCP `socket_owners`) | `peer.rs` | **fails** | pty tie is narrowing-only after ADR 0005 Q10; board TCP attribution is §9.3 |
| `cwd` | `lane::pane_cwd` (board "what dir is this pane in") | **fails** | `agent-exec inspect` — or degrade to last-recorded cwd and label it |

## 7. Decision 6 — provider CLIs and credentials

With `HOME=~cadence-agent` every provider CLI keeps its native paths
and needs no cadence-specific handling:

| Provider | Login state under `~cadence-agent` | One-time provisioning |
|---|---|---|
| Claude | `.claude/`, `.claude.json` | `sudo -iu cadence-agent claude auth login` (interactive, once) |
| Codex | `.codex/auth.json` | `sudo -iu cadence-agent codex login` |
| Devin CLI | `.config/devin/`, `.local/share/devin/credentials.toml` | `sudo -iu cadence-agent devin auth login` |
| Cursor | `.config/cursor/auth.json`, `.cursor/` | `cursor-agent` login, once |
| pi | `.pi/agent/auth.json` | copied or logged in once |
| `gh` | `.config/gh/` — the §4 F1 bot credential | provisioned file, not a login flow |

Rules this decision carries:

- **One login per provider, shared by all lanes of that provider.** It
  is the agent *system*'s credential, not the operator's; the
  subscription accounts are the same vendor accounts, signed in a
  second time under a second home — the same shape as an operator
  logging in on a second machine.
- **Nothing is copied from the operator's home.** ADR 0004 warned that
  copying credentials multiplies secrets; provisioning is a fresh login
  per provider, so revocation is per-home and clean.
- **The master's `CLAUDE_CONFIG_DIR`** (`<state>/master/claude`, `0700`,
  Landlock-guarded, per `master::confinement` /
  `docs/design/AGENT-FILESYSTEM.md`) lives inside the private state dir
  — unreachable once the master's provider runs as the agent uid. The
  master's config moves to `~cadence-agent/master/` (`0700`), and the
  confinement read-set changes accordingly. The master's Landlock
  policy is otherwise unchanged — Landlock restricts what a process
  *reaches*, orthogonal to uid; it stays the read backstop under the
  new uid.
- **Daemon-side secrets never move.** `CLOUD_SECRET_ENV`
  (`src/adapter/mod.rs`) already strips Devin-cloud credentials from
  pane env; custody (PR #275) stays daemon-side by construction.

## 8. Decision 7 — build caches and disk

| Cache | Today | Under separation | Decision |
|---|---|---|---|
| Shared dep cache `<repo>/.cadence/target/shared` | all lanes, one uid | cross-uid | Keep shared — it lives inside the agent-owned store (§4), where the `cadence` group + default ACL already cover it; uid-1000 lanes during the transition keep the operator checkout's own cache. `project.yaml`'s `build.target_dir: per-worktree` remains the escape. |
| Per-worktree `target/` | lane-local | same | Unchanged — owned by whichever uid the lane runs as. A lane flipped mid-life re-creates its worktree under the store (§10), so the old `target/` is discarded with it — `cargo clean` in the worst case. |
| sccache | server on `127.0.0.1:4226` as `ubuntu`, 10 GB cache | TCP crosses uid | Works as-is: the daemon's job is only to give the agent uid a *client* (`/opt/cadence/bin/sccache` or a per-agent `cargo install`). Residual named: a hostile agent can write cache entries — same exposure the shared dep cache already has; cross-uid does not worsen it. |
| `cargo`/rustup toolchain | `~/.cargo`, `~/.rustup` | unreachable | **Per-uid install** at provision (`rustup` into `~cadence-agent`): ~3–5 GB, zero coupling. Alternative noted: `RUSTUP_HOME=/opt/rustup` shared read-only with operator-owned updates — rejected for stage 1 because a shared toolchain turns every toolchain upgrade into a cross-uid coordination; revisit if disk bites. The agent's `~/.cargo/config.toml` does **not** inherit the host `rustc-wrapper` line unless the sccache client path is agent-visible. |
| pnpm/npm store | `~/.local/share/pnpm`, `~/.npmrc` (0600, token-bearing) | unreachable | Per-agent store in `~cadence-agent`; `~/.npmrc` is **not** copied — if a private registry is ever needed it gets an agent-scoped token, never the operator's. Public-registry installs need none. |
| `/tmp` scratch | shared | shared | Agents get `/tmp` like everyone; lane temp dirs (`/tmp/<lane>` per the test rules) are created agent-uid anyway once the suite runs agent-side. |

## 9. Decision 8 — operator proof after separation

This is the point of the exercise, so it is written as a before/after:

**9.1 The caller rule.** `check_peer` admits `{euid, agent_uid}`; caller
derivation gains the peer uid as a kernel fact. The rule becomes:

| Peer uid | Derives as | Can ever be `Who::Operator`? |
|---|---|---|
| agent uid | agent family — alias by pane ancestry or enrollment, else `Unproven` | **no — kernel boundary** |
| daemon uid | as today: `operator_proof` narrowing, then a grant (ADR 0005) | only with a grant |
| anything else | `Unproven`, refused for writes | no |

`Unproven` stays "public reads only" per ADR 0005 P1. Nothing in the
grant machinery changes: the key, the statements, the TTLs, the
delegations are identical — uid separation does not replace the factor,
it walls the factor's material off from agents.

**9.2 Which guardrails become boundaries.**

| ADR 0005 residual / shape | Mechanism under one uid | Under separation |
|---|---|---|
| R7 live authority — rewrite `allowed_signers`, restart a patched daemon | file owned by the same uid; daemon signalable | **Closed.** The file, the store and the daemon are a different uid: unreadable, unwritable, unsignalable. The records half was already closed by committed signers; this closes the live half. |
| R2 — `send-keys` into an unlocked operator terminal | same tmux server uid | **Closed.** `/tmp/tmux-1000` is unreachable to agent uid. (The unlock TTL's lend-within-a-session is unchanged — an *operator-launched* process in that session still inherits it, which is the accepted trade.) |
| R3 — trojaned `cadence`/`ssh-keygen` on PATH | uid-owned symlink | **Narrowed.** `/opt/cadence` is `ubuntu`-owned; the operator's own PATH entries are unreachable. An agent can still trojan *another agent's* PATH inside the shared agent home — an intra-agent residual (§12), not an operator path. |
| Every §1.3 detach shape | shapes reproduce the heuristic | All still run — as agent uid, which can now never satisfy `Who::Operator` regardless of shape. The shapes stop mattering for operator authority; they still matter for *alias* attribution, which ancestry/enrollment handles. |
| The `0600` operator secret / custody store | one `cat` away | An actual boundary: `0700` dir + foreign uid. ADR 0006 §1.5 P4 closes as a residual. |

**9.3 What still needs the operator's key — and what does not improve.**

- Every human-class act: unchanged, per-act signatures.
- The unlock grant: unchanged. uid-1000 processes that are not
  proven-operator keep needing it; the daemon cannot tell an operator
  shell from an operator-domain daemon child — same trust domain, as
  today.
- **The board's TCP attribution needs one explicit branch — today an
  agent-uid peer resolves as `Foreign` *or* `NoAgent`, and a live
  session maps both to `Operator`.** Two arms share the hole on the
  pinned commit. `tcp_peer_agent`/`socket_owners` (`peer.rs`) find no
  visible owner for an agent-uid socket (`/proc/*/fd` is
  cross-uid-opaque) → `Err` → `attribute` (`ui/operator.rs`) recovers
  the socket's owner *uid* from the `/proc/net/tcp` uid column —
  readable cross-uid — and classifies it `Attribution::Foreign`. And
  with zero live agent roots, `attribute`'s `roots.is_empty()` arm
  returns a live client socket as `Ok(None)` → `Attribution::NoAgent`
  (operator.rs ~462–478). `decide` (operator.rs:425) maps `(session,
  Foreign)` *and* `(session, NoAgent)` → `Verdict::Operator("operator
  (ui)")` — designed for tailscaled/sshd proxies, but under separation
  the *agent uid is another foreign uid*: an agent-uid process
  presenting a live operator session writes as `operator (ui)` —
  including a straggler while the registry reads empty — and the
  `Stolen` revocation (same-uid today) cannot fire. Separation is
  net-positive here — the session material itself becomes unreadable
  (private state dir), so this needs a *stolen* session, not a
  borrowed one — but the hole is named, not excused: the agent-uid
  check belongs at the socket-uid recovery point, so a client socket
  owned by the configured agent uid classifies agent-family (refuse;
  revoke a presented live session — `Stolen` without an alias)
  *whether attribution arrived via `Err→Foreign` or via
  `Ok(None)→NoAgent`*. `route_class`'s unlisted-writes posture (fail
  closed) and `tcp_peer_operator_proof`'s empty-owner fail-closed stay
  unchanged.
- Agents' board *reads* stay public; agents' board *writes* die in
  favour of the daemon socket, which is the correct topology anyway.

## 10. Decision 9 — migration, observe phase, rollback

**Stage A — provision (root, once, reversible).** §5's provision list,
plus provider logins (§7), the `gh` bot credential (§4) and the
per-repo store clones (§4). Ends with `cadence doctor` reporting the
uid, groups, shared dirs, the store, the helper and agent-uid logins —
and asserting the negative: `/home/ubuntu` carries no ACL or grant
reachable by the agent uid. **Nothing launches differently yet.**

**Stage B — code, default off.** `agent_uid` is a per-state-dir config
(`setup`/state record, absent ⇒ today's identical same-uid behaviour —
tests, CI and other hosts are untouched). The code paths behind it:
socket dual-bind + `check_peer` admit set; `pane_env` env rebuild
(`CADENCE_SOCKET`, `CADENCE_PM_DIR`, clean `PATH`, `HOME`,
`CADENCE_SUITE_LOCK`, drop `CADENCE_STATE_DIR`); pane command wrap;
stdio/ws spawn wrap; `kill`/`inspect` routing for agent-uid pids;
briefing write into the lane; the §4 store — `create_worktree` gets a
per-lane-mode root (`repos/<name>` for agent-uid lanes) and every git
against `/var/lib/cadence` from a non-agent caller execs through the
helper (`commit_who`, `worktree::add`, store fetch/prune); board
interim rule from §9.3.
Fail-closed launch: `agent_uid` configured but helper/uid/dirs missing
⇒ refuse to launch, loudly — never silently fall back to operator uid.

**Stage C — observe (one lane, real work).** One worker alias is
registered with the agent-uid launch flag — *before the helper exists
this can be done with L3 (`sudo -n -u`) since NOPASSWD already
permits it*, which is precisely why the observe phase is cheap to try
and explicitly not the shipped mechanism. The lane runs an ordinary
ticket end to end: briefing read, pane turns, `cadence self`,
`issue`/`message` writes via the socket, `cargo build` against the
shared dep cache and sccache, `git commit`/`push`, `gh pr create`,
operator attach + `send-keys`, stall watch, `agent stop` + reap.
Success bar: a merged PR authored in that lane, plus a day without a
"cannot reach" event.

**Stage D — default on.** The launch flag defaults to agent-uid for new
panes; running uid-1000 panes are recycled naturally (agents stop
constantly) or by `agent stop` + relaunch. Managed endpoints cut over
in the same flag's second step after pane stability — stdio/ws spawns
have no tmux buffer, so they go last, not first.

**Rollback.** Per stage: C→B is flipping one lane's flag back; D→B is
flipping the default — inert helper, inert uid, zero state to undo;
B→nothing is removing the config row. The tracker move (§3.2) rolls
back as a `mv` plus the symlink swap — cheap, scripted, rehearsed
during stage A — and the repo store is purely additive: the operator's
checkout never moved, so rolling it back is deleting
`/var/lib/cadence/repos`.

**What breaks for existing worktrees, honestly:** nothing under `~` is
reachable post-cutover — there is no ACL at all — so a pre-existing
`~/Project` worktree is *closed* to an agent-uid lane, not
half-writable. The flip is: push the lane's branch, retire the old
worktree, let `issue start` re-create it under `repos/<name>`;
unmerged work moves through the forge like everything else. `op-pm`
itself is an agent: its cutover is one lane like any other —
scheduled late, since the PM's breadth of filesystem habits is the
widest.

## 11. Questions for the operator

1. **The setuid helper (L1) vs the supervisor (L2).** Recommendation is
   L1 — with the honest cost on the table: a *correct* helper only ever
   crosses `ubuntu`→`cadence-agent` (to a uid with nothing), but a
   *defect* in a setuid-root binary runs attacker code as **root**
   before the privilege drop — that is the artifact being accepted,
   which is why its design is minimal and T2's adversarial list exists.
   On this host the caller already has NOPASSWD sudo, so the helper
   adds no privilege path *here* — a fact that must not be generalised
   to other hosts. Choose L2 — a Rust-only audit surface, no setuid
   artifact at all — if setuid-root is a hard no regardless of target.
2. **One uid (U1) vs a per-lane uid pool (U2).** Recommendation: U1 now,
   U2 as a follow-up ticket if intra-agent isolation is wanted. If the
   operator wants U2 in the same deployment, the launch helper takes
   the target uid from config either way — the difference is
   provisioning machinery, not mechanism.
3. **The `gh` credential (F1):** a fine-grained PAT on a bot/machine
   account scoped to `favcrm/cadence`, or a GitHub App installation
   token? (The ADR recommends the narrowest available: App token if the
   org allows one, PAT otherwise.) What scopes beyond `contents:write`
   + `pull_requests:write` are actually needed — do lanes `gh pr
   merge`/`--admin`? If never, withhold them.
4. **The observe lane:** which alias and which ticket? (Recommendation:
   a fresh `swe` lane on a docs-or-test ticket — low blast radius, real
   exercise of every surface.)
5. **Names and paths:** `cadence-agent` / `cadence` / `cadence-launch`;
   `/var/lib/cadence`, `/opt/cadence`. Alternatives welcome; the ADR
   deliberately avoids `sudo` in the shipped path.
6. **Ubuntu's own NOPASSWD sudo:** out of scope here (the design does
   not rely on it — that is the point of the helper). Should stripping
   or narrowing it be a follow-up ticket once agents no longer need it?
   (It only becomes meaningful once agents are off uid 1000, so it
   sequences after stage D.)
7. **The §4 sharing decision.** Recommendation: the agent-owned store —
   nothing under `~` is ever ACL'd or shared, and git's own
   dubious-ownership refusal keeps operator-uid git out of the
   agent-writable `.git`s. The alternative is R1's traverse ACL plus a
   recursive `o`-strip lockdown sweep and a standing `doctor` assertion
   that no agent-reachable `o+r` path under `~` survives — a permanent
   obligation on a mixed-content home, which is why the ADR recommends
   relocating instead. (Sharing `.git` itself was examined and fails on
   git's need for top-level `.git` write — there is no partial-trust
   shared `.git` to offer as a third way.)

## 12. Residuals after this design (honest list)

A `cadence-agent`-uid adversary — one compromised or prompt-injected
agent — can still:

- **Read and write every lane's worktree** and the shared tracker,
  dep cache, repo store and `/var/lib/cadence/lanes` — shared uid means
  no intra-agent boundary (U2 is the fix; §11 Q2).
- **Rewrite the shared stores' `.git` — config, hooks, filters.** Buys
  nothing new: by §4's invariant those dirs only ever execute git as
  the agent uid, so a planted hook's blast radius is the agent domain
  itself — an intra-agent matter, not an operator channel.
- **Signal sibling agent processes** (`kill`, same-uid), including the
  PM lane. Mitigations today: none new — `pane_pid` + starttime
  verification still bounds what the *daemon* kills; the agent-side
  signal is accepted, as it is today.
- **Read `/proc/<peer>/environ`** (same-uid allowed) — bearer material
  in pane env (turn tokens on `message_report`'s `Rule::Bearer`) is
  sniffable from a peer's env. Pre-existing same-uid property,
  unchanged by this design; hardening option for a later ticket: bind
  bearer tokens to the reporting pane's recorded pid.
- **Use the shared `gh` bot credential** for any repo write it scopes —
  branch pushes across lanes, PR opens. Narrower than today's shared
  operator login; still shared.
- **Poison shared caches** (dep artifacts, sccache) — same exposure as
  today, unchanged.
- **Run its own tmux server / detach shapes** — now strictly *less*
  useful: none of them reach operator material or authority.
- **Read every provider session in `~cadence-agent`** — one shared home
  means one lane reads another lane's Claude/Codex transcripts and
  config. Accepted for U1; U2 fixes.

What it **cannot** do, that it can do today: read or write the daemon's
state, custody store, operator signers, `operator/secret`, logs; signal
or ptrace operator-uid processes; reach any `/tmp/tmux-1000` socket
(including cadence's own — closes CAD-288's class); `sudo`; read
*anything* under `/home/ubuntu` — no ACL is granted, so the `0750`
home stays absolute; plant code where an operator-uid git will run it
(the only `.git`s it can reach are agent-owned, and foreign-uid git is
refused there); see the operator's `gh`, ssh, npm or provider
credentials; satisfy `Who::Operator` in any process shape.

## 13. Implementation tickets (proposed, for the epic's owner to file)

Each is one PR, risk class `human`, with an independent adversarial
review pinned to head SHA — per the gate rule: every boundary enforced
here gets a test that fails without it (an agent-uid caller mutating
the state dir, a forged uid field, a helper argv outside its allowlist,
a detached child under the agent uid, a foreign-uid git refused inside
the agent store).

| Ticket | Scope |
|---|---|
| T1 — provision verb + host provision | `cadence agent-uid provision` (the §5 script), doctor checks for every artifact — including the negative assertion that `~` carries no agent-reachable ACL — then the operator runs it once on this host |
| T2 — `cadence-agent-exec` helper | the three verbs, env allowlist, caller-group check; adversarial tests (argv injection, env smuggling, signaling a foreign-uid pid refused by kernel) |
| T3 — socket split + uid admit set | dual-bind, `CADENCE_SOCKET`, `check_peer` admit `{euid, agent_uid}`, peer uid into derivation; board interim rule (§9.3) |
| T4 — pty launch under agent uid | pane command wrap, `pane_env` rebuild, briefing into lane, kill/inspect routing in `reap_session`/`kill_pane`/`pane_cwd` |
| T5 — managed endpoints under agent uid | stdio/ws spawn wrap + signal routing; `Enrollment` uid asserted |
| T6 — relocation | tracker move + symlink (agent-owned `.git`), binary prefix, `master`'s `CLAUDE_CONFIG_DIR` move, the per-repo agent stores, `create_worktree`'s lane-mode root, and the shared-git wrapper — every git against `/var/lib/cadence` from a non-agent caller execs through the helper |
| T7 — observe lane, then default-on | flag → one lane → default; per-lane worktree re-creation under the store; the `op-pm` cutover last |
| T8 — close-out | ADR 0005 §12 Q1 addendum: Option D live, R7/R2 rows flipped; `docs/AUDIT.md`, `docs/BOARD.md`, `AGENTS.md` updated; AGENT-FILESYSTEM enforcement table's same-uid caveats re-stated |
