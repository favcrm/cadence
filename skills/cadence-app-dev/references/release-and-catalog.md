# Toolchain, Validation, Catalog & Artifact Release Workflows

Never run state-changing install, upgrade, release, binding, approve, revoke, or effect commands against a workspace or hosted `--org` without explicit user authorization.

## 1. Consuming `@cadence/app-kit` and `@cadence/screen-ui`

Detached app repos pin the versioned toolchain release asset:
- Download `cadence-app-toolchain-<tag>.tar.gz`, `.sha256`, and verify checksum + provenance attestation before updating a repo's pinned archive.
- Install dependencies reproducibly inside the app worktree with `bun install --frozen-lockfile`.
- Check `@cadence/screen-ui/DESIGN.md` inside the installed package (it is packaged and hashed into archive provenance). Note: adding or reading docs in the `cadence` repo does not upgrade already-pinned archives in downstream app repos.
- Access this bundled skill anywhere on a host with Cadence installed:
  - Inspect: `cadence skill show cadence-app-dev` (or pass a reference path)
  - Install/link for agents (`.claude/skills`, `.cursor/skills`, `.copilot/skills`, `.gemini/skills`): `cadence skill install cadence-app-dev`

## 2. Offline Validation, Frame Kit & Fixtures

| Command | Purpose | Key Rules / Caveats |
| :--- | :--- | :--- |
| `cadence app dev --package .` | Serve app locally via `package.json.cadence` | Scrubbed `CADENCE_APP_*` env; ports `3110..=3199`; zero shell metacharacters in `cadence.dev`/`check`. |
| `cadence app check <source> --json` | Offline install check (grammar, caps, manifests, descriptors, workflows, `requires`) | Exit `0` pass, `3` refusal. Does not check duplicate installs or pending catalog journals. **Never ship solely on `inspect` or `--integrity-only` preview/fixture modes.** |
| `cadence app inspect <source> --json` | Inspect bundle integrity & observed Native requirements | Read-only; does not claim target compatibility and installs/approves nothing. |
| `cadence app frame-kit --out <dir> --bundle <dir> --parent-origin <origin>` | Export exact host screen frame + kit CSS for offline preview | Refuses existing `--out`; `--parent-origin` must be `https://...` or `http://localhost:<port>` / `http://<name>.localhost:<port>`. `--integrity-only` skips target compatibility check (static preview only). |
| `cadence app fixtures --bundle <dir> --json` | Emit canonical screen `PUSH` fixtures from validated bundle | Synthetic fixtures only; `--integrity-only` skips target runtime compatibility. |

Note: `builtin:<id>` package sources are retired (`CAD-1422`) and refused by the CLI and daemon. Pass a local directory or canonical Git repository URL.

## 3. Local Workspace Catalog vs. Hosted (`--org`) Catalog

### Local Workspace Catalog (`cadence app catalog ...`)

- `cadence app catalog install-check <source>` — read-only preflight returning resolved digest, version, and file list.
- `cadence app catalog install <source> --expected-digest <digest>` — install local/git workspace bundle (one workspace install per app name; does not grant capabilities until approved).
- `cadence app catalog ls` / `cadence app catalog show <install-id>` — inspect installations, `catalog_generation`, `digest`, and `approval`.
- `cadence app catalog upgrade-check <install-id> <source> --expected-digest <old> --expected-generation <gen>` -> `cadence app catalog upgrade ... --expected-new-digest <new> --request-id <id>` — digest-pinned atomic replacement.
- `cadence app catalog approve <install-id> --digest <digest>` / `revoke <install-id> --digest <digest>` — operator gate for local artifact execution.
- `cadence app catalog recover <install-id>` — resume interrupted install journal under `.apps/install-journals/` (distinct from `migration-recover <journal-id> [--rollback]`).
- `cadence app catalog migrate` — explicit migration from legacy per-project installations before using workspace installs.

### Hosted Org Catalog (`cadence --org <slug> app catalog ...`)

Selected remote orgs never accept local bundle uploads or fall back to local catalog operations:
- `release-check <listing> --version <v> --digest <d> [--install-id <id> --expected-digest <d2> --expected-generation <g>]` — inspect immutable published release and write review JSON.
- `release-install <listing> --version <v> --digest <d> --operation-id <op> --review <review.json>` — install consented release pinned to exact `release-check` JSON output (`<= 128 KiB`).
- `release-update <install-id> <listing> --version <v> --digest <d> --expected-digest <old> --expected-generation <g> --operation-id <op> --review <review.json>` — upgrade pinned installation.
- `release-revoke <install-id> --digest <d> --expected-generation <g> --operation-id <op>` — revoke approval on remote install.
- `operation <operation-id>`, `pending <app>`, `reconcile <operation-id>` — durable receipt inspection and safe reconciliation (never redispatches mutations).

## 4. Artifact Slots, Bindings & Release Effects

Apps publishing content or requiring host asset storage declare `needs.capabilities` slots (such as `publication_slot`, `required_asset_slot`) and go through explicit operator bindings + two-step effect approval:

- **Bindings:** `cadence app binding create|ls|show|set|publish|revoke <install-id> ...` (revision-pinned connection/destination configuration).
- **Effects:** `cadence app effect stage <run-id> --artifact-id <id> --slot <slot> --request-id <req> --title <title>` -> `show <effect-id>` -> `accept <effect-id> --digest <digest>` (or `decline` / `resolve --resolution close|acknowledge`). HTTP transport caps effect request bodies at `48 KiB` and receipts at `4 MiB`.
