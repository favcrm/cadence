# App local development

Detached app repositories own their screens, workflows, preview tooling and
toolchain. Cadence provides one entrypoint, the package contract and the
refusal boundary; it does not copy the app's renderer or install anything.

## Contract

An app repository declares a `cadence` object in its `package.json`:

```json
{
  "cadence": {
    "app": "social-content",
    "dev": "node scripts/dev-preview.mjs",
    "check": "scripts/pre-push"
  }
}
```

- `app` is the package identity (1–64 lowercase `[a-z0-9-]` characters).
- `dev` is one long-running preview command, parsed into arguments (quotes
  are supported; shell expansion and globbing are not). Leading
  `NAME=value` assignments apply to its environment. It owns its port,
  renderer and fixture data. Put multiple steps in a script: `;`, `&&`,
  `||`, `|`, redirects, backticks and command substitution are refused so
  Cadence can `exec` the command and signals reach its cleanup.
- `check` is optional; when present it runs to completion before `dev` and a
  non-zero exit stops the preview.
- The package must contain `app/app.md`.

The command is trusted local development tooling. It does not install,
approve, deploy or call a daemon, and the app command must keep its preview
on loopback unless the app deliberately adds a supported sharing mode.

## Run

From the app repository:

```sh
cadence app dev --package .
```

The command validates the contract, runs `check` when declared, then replaces
itself with the app's `sh -c dev` command. Terminal and service signals reach
that command, so its own cleanup must stop any child server. It passes a
scrubbed environment:

| Variable | Value |
|---|---|
| `CADENCE_APP_ROOT` | canonical package root |
| `CADENCE_APP_NAME` | `cadence.app` |
| `CADENCE_APP_PORT` | `--port` (default `3186`, range `3110..=3199`) |
| `CADENCE_APP_HOST` | `--host` (default `127.0.0.1`; only loopback is accepted here) |
| `CADENCE_APP_CONTRACT` | `1` |

Only `PATH`, `HOME`, `LANG`, `TERM`, `PNPM_HOME`, `XDG_CACHE_HOME` and
`TMPDIR` are inherited. No `CADENCE_STATE_DIR`, PM path, daemon socket,
provider key or operator session value is exposed.

`--package` refuses `--allow-host` and a non-loopback `--host`; it also
refuses a missing `package.json`, `app/app.md`, `cadence.app` or
`cadence.dev`.

## Legacy host harness

The previous mode remains for host-owned previews:

```sh
cadence app dev social-content --source /path/to/cadence --port 3186
```

It runs the trusted checkout's `scripts/app-dev.mjs` against
`app-previews/<name>`. New app repositories should use `--package`.

## What this is not

- Not an install, approval or effect path.
- Not a host renderer or SDK; the app's `dev` command owns its imports.
- Not a container or remote preview; use the app's own runner or the existing
  container preview for that boundary.
