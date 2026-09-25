# ADR 0007 runbook — the operator's one-time root provisioning

Audience: the operator (`ubuntu`, uid 1000). Timing: **after T2's
independent adversarial review passes on the merged head** — provision
is risk class `human`, and this runbook is the act it reviews.

Nothing on the host changes before you run step 3. No install path,
startup hook, or ordinary `cadence` invocation touches users, groups,
`/var/lib/cadence` or `/opt/cadence` — `agent-uid provision` is the only
verb that writes them, and it refuses to run unless its real and
effective uid are both 0.

## 0. Preconditions — all must hold before step 3

- T2's review on the merged head SHA has passed. Do not provision
  against an unreviewed head.
- You are on the host as `ubuntu`: `id -u` → `1000`.
- The negative assertions are already clean. They are checked in the
  *unprovisioned* state deliberately — a violation here is an operator
  fix, not a provision flag:

  ```sh
  cadence agent-uid doctor
  ```

  Expected: `agent-user`/`agent-groups`/artifact rows `warn`
  ("not provisioned"), `home-acl` and `git-config` `ok`. A `fail` on
  either negative row — an ACL under `~` granting the agent domain, or a
  `safe.directory`/`include.path` covering `/var/lib/cadence` in any
  uid-1000 git config — means fix that first; provision refuses anyway
  if it finds one.

## 1. Build

From the reviewed checkout:

```sh
cargo build --release --bin cadence --bin cadence-agent-exec
```

## 2. Preview — print every action, change nothing

```sh
sudo ./target/release/cadence agent-uid provision --dry-run \
    --helper "$PWD/target/release/cadence-agent-exec"
```

`--helper` is required and must be absolute — the file it names is the
setuid bridge the boundary rests on, so provision never resolves one
from the cwd: under sudo the cwd may be agent-writable, and a binary
dropped there would be installed setuid-root.

Read the printed lines against ADR §5: `groupadd` ×3, `useradd
--system`, `usermod -aG` ×3, `install -d` per directory with its §3
mode, the helper at `root:cadence-launch 4750`, the repos default ACL.

## 3. Provision — once, as root, idempotent

```sh
sudo ./target/release/cadence agent-uid provision \
    --helper "$PWD/target/release/cadence-agent-exec"
```

The source is vetted before a byte moves: a symlink, a fifo or
anything else that is not a regular file, a group/other-writable
file, or an owner that is neither root nor the operator refuses the
run — and the install re-verifies the copied bytes, so what lands at
`/opt/cadence/libexec/cadence-agent-exec` is provably the file you
named.

What it does, in order — each step checks before it acts, so re-running
is a no-op:

| action                                              | spec                                   |
| --------------------------------------------------- | -------------------------------------- |
| `groupadd --system cadence-agent`                   | the account's primary group            |
| `groupadd cadence` / `groupadd cadence-launch`      | the share edge / the launch edge       |
| `useradd --system -m -d /home/cadence-agent -s /usr/sbin/nologin -g cadence-agent cadence-agent` | locked system account |
| `usermod -aG cadence ubuntu` / `usermod -aG cadence cadence-agent` / `usermod -aG cadence-launch ubuntu` | the only cross-uid edges |
| `/home/cadence-agent`                               | `cadence-agent:cadence-agent 0750`     |
| `/opt/cadence`, `/opt/cadence/libexec`              | `root:root 0755`, `root:cadence-launch 0750` |
| `/opt/cadence/libexec/cadence-agent-exec`           | `root:cadence-launch 4750` — the only operator→agent crossing |
| `/var/lib/cadence`, `…/lanes`                       | `ubuntu:cadence 0750`                  |
| `/var/lib/cadence/repos`                            | `cadence-agent:cadence 2750` + `d:g:cadence:rwx` |
| `/opt/cadence/bin`, `/opt/cadence/releases`         | `ubuntu:cadence 0755`                  |

Between the account and filesystem phases the verb re-runs §4's
negative assertions as a pre-flight and **refuses** if either fails.

What it never does, by construction — the action set has no verb for
it: write under any home, write any git config (`safe.directory`,
`include.path`), touch sudoers, or run on an unprivileged caller.

## 4. Verify

```sh
sudo ./target/release/cadence agent-uid doctor   # expect: level ok
cadence doctor --host                          # agent-uid row
```

Run `agent-uid doctor` as root here: the `agent-user` row reads
`/etc/shadow` to prove the account's password is locked, and an
unprivileged seat cannot — from `ubuntu` the row reports `warn`
("password lock unverifiable") even on a correctly provisioned host.
The `doctor --host` row shares that ceiling: expect `ok` from a
root-capable caller, `warn` with the same detail from uid 1000. Any
other `fail`/`warn` row means what it says — a drifted mode, a stray
group member, a missing setuid bit — and carries its remedy line.

## 5. Acceptance — the helper's live proofs, run for real

The `agent_exec` suite is `#[ignore]`d by default and skips loudly on an
unprovisioned host. In the runbook a skip is a failure: gate it, and
give the fd-sweep test the headroom it needs to prove the inherited-fd
invariant — it must be able to seat fd 65537, above the pre-CAD-522
fallback clamp.

```sh
ulimit -n 65538     # RLIMIT_NOFILE above the seat; sudo keeps it
sudo -E env CADENCE_PROVISION_RUNBOOK=1 \
    cargo test --test agent_exec -- --ignored --test-threads 1
sudo -E env CADENCE_PROVISION_RUNBOOK=1 \
    cargo test --bin cadence-agent-exec -- --test-threads 1
```

- The `--bin` run covers the helper's own unit proofs — including
  `close_fds_has_no_65536_clamp` and `procfs_sweep_has_no_65536_clamp`,
  which pin the unclamped fd sweep the `--test agent_exec` run
  exercises end to end.
- `CADENCE_PROVISION_RUNBOOK=1` turns every SKIP into **exit 42** — the
  runbook treats exit 42 as a failed acceptance, never as "nothing to
  test". If the sweep cannot seat fd 65537 the binary exits 42; raise
  the limit and re-run, do not accept the green.
- Each test still requires the provisioned host — the gating changes
  what a skip *means*, not whether one can happen.

## Rollback

```sh
sudo userdel -r cadence-agent          # removes /home/cadence-agent
sudo groupdel cadence-agent cadence-launch
sudo groupdel cadence                  # only after cadence-agent leaves it
sudo rm -rf /opt/cadence /var/lib/cadence
```

Repositories under `/var/lib/cadence/repos` are agent work — decide
their fate explicitly before removing the tree.
