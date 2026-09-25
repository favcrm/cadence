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

Tagged releases will ship `install.sh` with checksums and build-provenance
attestation once the first `v*` tag is cut; until then there is nothing at
`releases/latest`.

## Quick start

```bash
cadence setup                  # idempotent first run: state dir, tracker,
                               #  skill, daemon and board; reports provider CLIs
cadence daemon start           # detached controller
cadence skill install          # ship the `cadence` skill to your agent CLIs

cadence claude                 # managed headless Claude — or devin / codex / cursor
cadence join <pm> devin        # spawn a worker wired to a group PM
cadence dispatch CAD-1 --to w1 # one-step issue hand-off

cadence status                 # one-screen fleet overview
cadence ui run                 # the board: 127.0.0.1:3010
```

`cadence --help` is the full reference; `docs/CLI.md` in this checkout keeps
the long-form catalog, and `docs/SESSION.md` describes the PM loop
(dispatch → review → verdict → merge).

## What it gives you

- **Durable messaging** — every assignment, acknowledgement and result is a
  persisted message; delivery and reporting survive restarts.
- **Real endpoints** — owned tmux panes for TUI providers (Devin, Cursor,
  Claude `--tui`) and managed headless endpoints (Codex ws, Claude
  stream-json); sessions resume by their native ids.
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

The initial release targets Linux. Provider CLIs and their authentication remain
external dependencies; native terminal integration may require tmux.

## License

MIT. This project is not affiliated with provider vendors or other projects named Cadence.
