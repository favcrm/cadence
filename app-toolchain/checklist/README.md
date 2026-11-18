# Private checklist example

A small hosted workspace app: list tasks, add a task, and mark one complete.
The screen uses `@cadence/app-kit` and asks the host for declared record
operations. Task data stays in the host's per-installation storage; the app
has no database credentials or provider secrets.

Keep this directory, `template/` and `app-kit/` as siblings from the verified
app-toolchain archive. With Bun and a matching Cadence CLI that supports
`app inspect`:

```sh
cd checklist
bun run package
```

Choose the ZIP printed under `dist/` in the workspace's Apps → Add private app
flow. A workspace owner must review and install it; packaging alone installs
nothing and does not prove hosted compatibility or absence of secrets. The
host checker is authoritative for the target workspace. `app-storage/v1` is
hosted functionality; the Native preview may refuse it.

An authorized agent can discover this app with `app_describe` and read through
`app_call` using the declared record operations. Agent writes are proposals for
the owner's decision, not background writes. Change the app id before using
this example alongside another installation with the same name. If the storage
migration changes, update its SHA-256 in `app-storage.json` and follow the
host's versioned migration rules; never edit an already-applied migration.
