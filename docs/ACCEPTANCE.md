# Real-provider release acceptance (Tier B)

Run this checklist before declaring the MVP complete and for each release candidate. Use the **same source SHA and toy scenario** on clean Linux and macOS arm64. The scripted, fake-provider journey is Tier A; it is not evidence of provider sign-in, real dispatch, or recovery of operator context.

**macOS runtime is blocked on CAD-315.** Apple Silicon installation and version checks are supported, but the native setup, daemon and board are not. Record the macOS leg as blocked until the candidate supports those operations. An install-only pass or `--unconfined` is not a substitute.

## Before the run

- Use a dedicated, non-root Linux test user (sandbox tier S2) and a disposable macOS arm64 machine or VM with a clean test user. Do not use a shared production host, home directory, Cadence instance, or browser profile. Each user needs an isolated login for the real Claude master and the approved worker and reviewer routes. Keep the author, reviewer, and operator roles separate.
- Select the release tag and source SHA. Confirm that the corresponding Linux and arm64 macOS assets come from that SHA. Follow [installer provenance verification](INSTALL-AGENT.md) before running the installer. Do not let the installer select `latest` independently on each platform. Record the install source and asset digest.
- Have an operator-owned, disposable Git remote for each run, with its own test credentials and the intended merge checks enabled. Never point a project at the Cadence repository or shared application data. Reserve a free loopback board port in `3110–3199`; confirm nothing is listening on it. Use a fresh browser profile and a plain operator terminal on the test machine.
- Confirm the configured Claude model and login route. Do not copy a login from another account, switch providers, change model policy to get past a refusal, or add a spend cap. Do not use unconfined mode to pass the Linux run.

## 1. Install and set up

Set `CADENCE_QA_TAG`, `CADENCE_QA_SHA`, and `CADENCE_QA_PORT` to the verified tag, source SHA, and reserved port. Use the published GitHub release, or set `CADENCE_QA_ASSET_ROOT` to an approved HTTPS staging source or an owned absolute `file://` mirror of the verified installer and assets. The source must serve all files under the selected tag.

```sh
set -eu
: "${CADENCE_QA_TAG:?Set the verified release tag}"
: "${CADENCE_QA_SHA:?Set the full candidate source SHA}"
: "${CADENCE_QA_PORT:?Set the reserved loopback port}"
case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:aarch64) ;;
  *) printf 'Native Tier B is supported on Linux only pending CAD-315.\n' >&2; exit 2 ;;
esac
test "$(id -u)" -ne 0
test -z "${CADENCE_ALIAS:-}${CADENCE_PROFILE:-}${CADENCE_ORG:-}"
test ! -e "$HOME/.local/bin/cadence"
test ! -L "$HOME/.local/bin/cadence"
printf '%s\n' "$CADENCE_QA_SHA" | grep -Eq '^[0-9a-f]{40}$'
case "$CADENCE_QA_PORT" in 31[1-9][0-9]) ;; *) exit 2 ;; esac
asset_root=${CADENCE_QA_ASSET_ROOT:-https://github.com/favcrm/cadence/releases/download}
case "$asset_root" in https://*|file:///*) ;; *) exit 2 ;; esac
qa_root=$(mktemp -d /tmp/cad434.XXXXXX)
export CADENCE_STATE_DIR="$qa_root/state" CADENCE_PM_DIR="$qa_root/pm"
export PATH="$HOME/.local/bin:$PATH"
started_at=$(date +%s)
curl --proto '=https,file' -fsSL "${asset_root%/}/$CADENCE_QA_TAG/install.sh" -o "$qa_root/install.sh"
sh "$qa_root/install.sh" --version "$CADENCE_QA_TAG" --base-url "${asset_root%/}"
installed=$(cadence --version)
printf '%s\n' "$installed"
case "$installed" in
  *"+$CADENCE_QA_SHA") ;;
  *) printf 'Installed revision does not match the candidate.\n' >&2; exit 1 ;;
esac
cadence setup --local --state-dir "$CADENCE_STATE_DIR" --port "$CADENCE_QA_PORT"
cadence daemon status --state-dir "$CADENCE_STATE_DIR"
cadence ui status --state-dir "$CADENCE_STATE_DIR"
```

The setup handover must say **local** and name the isolated board. The daemon must be ready. Check the board is listening only on loopback before opening the URL. Abort if any path or port points outside this run. `started_at` marks the start of the measured install-to-first-plan interval; do not restart the clock after a failed first attempt. See [First run](FIRST-RUN.md) for the handover fields.

Create the same toy repository on both platforms. The seed contains no customer data or secrets:

```sh
fixture="$qa_root/toy"
git init -q -b main "$fixture"
git -C "$fixture" config user.name 'Cadence Acceptance'
git -C "$fixture" config user.email 'cadence-acceptance@example.invalid'
printf '# Cadence Tier B toy repo\n' > "$fixture/README.md"
git -C "$fixture" add README.md
git -C "$fixture" commit -qm 'Seed acceptance fixture'
```

Record the seed revision. Attach only the verified disposable remote and publish the seed to its `main` before creating the plan. Register the checkout as the local project `tier-b` with `cadence project new tier-b --repo "$fixture" --state-dir "$CADENCE_STATE_DIR"`. Confirm the board shows that project, not a hosted workspace.

## 2. Start the real Claude master

From the isolated operator terminal, run `cadence master start --provider claude --state-dir "$CADENCE_STATE_DIR"`. If it reports that a separate provider sign-in is needed, follow the emitted sign-in instruction in the test account only. Read `cadence master status --state-dir "$CADENCE_STATE_DIR"` after launch. Record the real provider, resolved model and effort, and a successful authenticated turn. A policy check that says the model is launchable is not evidence that Claude answered.

## 3. Ask for work

In the isolated board's Home chat, ask the master to propose, **not yet execute**, two tasks for `tier-b`: (A) add `greeting.txt` containing exactly `Hello, Cadence!` and a newline; (B) after A finishes, document `cat greeting.txt` and the expected output in `README.md`. Require observable acceptance for each and a dependency from B to A.

Stop the measurement when the plan card first has a persisted plan ID. Run `echo "$(($(date +%s) - started_at))"` in the setup shell to record elapsed seconds. Record the ID, acceptance and dependency from the saved plan, not just from assistant prose. Keep the board open for the next flow.

## 4. Approve the plan

Before approval, verify neither work task has a kickoff. In the plan card, approve as the operator; record the decision and its actor. The first task can now become ready. An agent accepting its own proposal, or any kickoff before approval, fails the run.

## 5. Observe dispatch

Let the master continue after approval. Record the first assigned worker, task and kickoff message. B must remain blocked until A satisfies its prerequisite. Set `left_at=$(date +%s)` in the setup shell, then close the board tab without signing out. Allow work and review to proceed while the board is closed.

## 6. Observe submit and review

For each task record the worker's result, its real commit SHA, and a separate review tied to the same head. Check the actual diff and file contents. A reported SHA without a retrievable commit, an author's self-review, or a review of a superseded head cannot pass. Set `worker_alias` to the recorded alias and use `cadence events "$worker_alias" --state-dir "$CADENCE_STATE_DIR"` to correlate sanitized event IDs or cursors with the board evidence. A worker's completion report is not a merge decision.

## 7. Decide the merge in Needs you

Return to the same browser profile after more than one hour and capture the **Since you left** card before clearing it or making a decision. In **Needs you**, find the merge decision for the reviewed toy PR and check the head and required evidence. The operator acts through the normal delivery route; verify the resulting commit on the toy repository's default branch before treating A as done. Observe the dependent task through the same dispatch, review and merge path. Never bypass the project's actual review, merge or operator checks. See [Board](BOARD.md) and [Delivery](guides/delivery.md).

## 8. Verify Since you left

Compare the saved card to what actually happened between departure and return: worker reports, ticket movement, and any decision still awaiting the operator. A pending merge is not a completed merge. Cross-check the interval with `cadence master summary --since "$left_at" --state-dir "$CADENCE_STATE_DIR"` (read only; do not add `--post`). A fresh browser profile, a fabricated last-seen value, or a CLI summary without the board card cannot pass the UI check.

## Record and close the run

Write `claudedocs/acceptance/<date>.md` with one section per platform. Record OS/architecture and isolation, candidate tag/SHA and asset digest, toy seed and resulting commit SHAs, resolved model and provider, run timestamps and elapsed install-to-first-plan seconds, and pass/fail/blocked with evidence for each flow. List every operator intervention and quote confusing prompts verbatim after redaction. Record provider-reported cost and currency or say that a per-run amount was unavailable; do not report it as zero. For failures include the stable refusal and sanitized event references. Keep raw logs and screenshots in the test environment; do not commit credentials, private paths, unredacted transcripts, or event data from other projects.

A candidate has Tier B evidence only when all eight flows pass on both platforms. Mark missing environments, unavailable provider routes, and operator refusals as blocked rather than skipped passes. Stop only this run's board and daemon with `cadence ui stop --state-dir "$CADENCE_STATE_DIR"` and `cadence daemon stop --state-dir "$CADENCE_STATE_DIR"`. Leave the production installation and processes untouched.
