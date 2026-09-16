# Cadence

Coordinate coding agents across terminals, from planning through verified delivery.

Cadence is an early Rust implementation of a local development-agent controller.
It is designed for a Codex or Claude PM coordinating Codex, Claude, Cursor and
Devin workers on one host, including terminals accessed over SSH.

**Development stage:** implementation in progress. Provider support is an
acceptance-tested capability, not a promise of universal session attachment.

See [the implementation plan](docs/IMPLEMENTATION-PLAN.md).

Keep task checkouts under `.worktrees/`; see [workspace setup](docs/WORKSPACES.md)
for creation, handover and cleanup instructions.

## Quick start

```bash
cadence daemon start            # detached controller (CADENCE_STATE_DIR sets the state dir)

cadence devin                   # fresh Devin TUI in an owned tmux pane, then attach
cadence devin -r cookie-cesium  # resume an existing Devin session, like `devin -r`
cadence codex                   # Codex managed-ws endpoint, then `codex resume --remote`
                                # (--detach opts out; non-TTY or inside tmux prints
                                #  the attach command instead of exec'ing it)

cadence join <pm-slug> devin    # spawn a worker wired to a group: results route to the PM
cadence attach [name]           # attach this terminal (alias, native id, or unambiguous
                                #  provider name); no name lists live attachable agents

cadence agent ready <slug>      # operator claim: pane inspected, idle, empty input
cadence message send <slug> --text "task"   # gated literal paste into the TUI
cadence agent attach <slug>     # print the tmux attach command (--run to exec)
```

A **group** is a PM agent plus its workers; the PM's slug is the group
handle. Workers join with `params.upstream` set to the PM's alias, which
makes their result reports route back to the PM's queue by default.

Everywhere a command takes an agent name, the provider-native session id
(Devin slug, Codex thread) resolves to the registered alias.

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
