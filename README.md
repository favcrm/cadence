<p align="center"><img src="ui/public/icon.svg" width="112" height="112" alt="Cadence icon"></p>

# Cadence

[![ci](https://github.com/favcrm/cadence/actions/workflows/ci.yml/badge.svg)](https://github.com/favcrm/cadence/actions/workflows/ci.yml)

Coordinate coding agents across terminals, from planning through verified delivery.

Cadence is an early Rust implementation of a local development-agent controller.
It is designed for a Codex or Claude PM coordinating Codex, Claude, Cursor and
Devin workers on one host, including terminals accessed over SSH.

**Development stage:** implementation in progress. Provider support is an
acceptance-tested capability, not a promise of universal session attachment.

## Install

Build from source — a recent stable Rust toolchain via
[rustup](https://rustup.rs) is all you need:

```bash
cargo install --path . --locked
```

[Cadence 0.1.0-beta.2](https://github.com/favcrm/cadence/releases/tag/v0.1.0-beta.2)
is available as an explicit-tag local CLI pilot for Linux x86_64/ARM64 and Apple
Silicon macOS. The installer checks archive/binary digests and version; the release
includes build-provenance attestations. Use the exact prerelease tag:

```sh
curl -fsSL https://github.com/favcrm/cadence/releases/download/v0.1.0-beta.2/install.sh -o install.sh
sh install.sh --version v0.1.0-beta.2
"$HOME/.local/bin/cadence" --version
```

Linux daemon/board evaluation uses fresh, separate state. macOS supports CLI
installation/version checks only; setup, daemon and board are unsupported pending
CAD-315. Full clean-platform setup/backup acceptance remains open under CAD-317.
There is no stable release: `releases/latest` does not select this prerelease.

See [native installation](docs/INSTALL-AGENT.md) for supported platforms,
explicit pilot versions and provenance verification. The
[release checklist](docs/RELEASING.md) records the first-publication gates.

## Quick start

```bash
cadence setup                  # idempotent first run: state dir, tracker,
                               #  skill, daemon and board; reports provider CLIs
cadence daemon start           # detached controller
cadence skill install          # ship the `cadence` skill to your agent CLIs

cadence claude --alias pm --role pm       # named headless PM
cadence join pm devin --alias w1 --detach # named worker; keep this shell free
cadence dispatch CAD-1 --to w1 --reply-to pm # use an existing issue ID

cadence status                 # one-screen fleet overview
cadence ui status              # check the board started by setup
```

Open the board URL printed by `cadence setup`. Use `cadence ui run` only when
deliberately serving a board in the foreground without one already running.

`cadence --help` is the installed CLI reference. Start with
[project context](docs/START-HERE.md) for contracts and source ownership, or the
[contribution guide](CONTRIBUTING.md) for development and validation.

## What it gives you

- **Durable messaging** — every assignment, acknowledgement and result is a
  persisted message; delivery and reporting survive restarts.
- **Real endpoints** — owned tmux panes for TUI providers (Devin, Cursor,
  Claude `--tui`) and managed headless endpoints (Codex ws, Claude
  stream-json, Pi rpc via `join <pm> pi`); sessions resume by their
  native ids.
- **A git-backed tracker** — issues are Markdown files in a private repo
  (`~/pm`); every write is one commit, `issue log`/`blame` work from git alone.
- **Verified delivery** — workers report with `sha:`; reviewers bind a verdict
  to that exact head; `cadence audit` flags merges with no passing verdict.
- **A board** — read/write SPA + JSON API on loopback; one writer
  implementation behind CLI and API alike.
- **A sandbox** — `cadence sandbox up <name>` runs a disposable second
  instance with its own state dir, tracker and port.

Task worktrees live under `.cadence/wt/` inside the repo checkout.

## Layout

```
src/        CLI, daemon and provider adapters (Rust)
ui/         the board (React + Vite; features/ + lib/ + ui/)
skills/     agent skills — cadence (installed by `cadence skill install`),
            agent-handover
agents/     master-agent seed files
contracts/  versioned platform schemas and worked vectors
workflows/  reusable workflow templates
tests/      Rust integration suites, e2e harness, fixtures
scripts/    install, CI and review helpers
docs/       working docs — the context-manifest set stays tracked
            (docs/cadence/project-context.yaml indexes it)
```

## Principles

- Preserve native session identity and visible terminal conversations.
- Separate terminal submission, explicit acknowledgement and verified work.
- Persist messages and events; stop on uncertain execution instead of replaying edits.
- Keep provider permissions intact and source state outside the repository.
- Review the exact worker revision before calling a task complete.

The release pipeline packages Linux x86_64/ARM64 and Apple Silicon macOS.
Packaging is separate from verified provider support on each platform.
Provider CLIs and their authentication remain
external dependencies; native terminal integration may require tmux.

## License

MIT. This project is not affiliated with provider vendors or other projects named Cadence.
