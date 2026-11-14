# Contracts, Manifests, and Storage (`app-storage/v1`, `app-chat/v1`, `screens.json`)

Always inspect the target repository's existing manifests and pinned contract versions before adding or editing files.

## 1. Root `package.json` `cadence` Block

Detached app repositories expose a `cadence` object in root `package.json` for local dev and verification (`cadence app dev --package .`):

- Required/standard fields: `cadence.app` (app identifier), `cadence.dev` (local dev command), `cadence.check` (verification command).
- **Command grammar rule:** Commands in `cadence.dev` and `cadence.check` must **not** contain shell metacharacters (`;`, `&&`, `||`, `|`, `<`, `>`, backticks, or `$()`). Point to a single binary or script file instead of chaining shell commands.
- `cadence app dev --package .` injects a scrubbed environment (`CADENCE_APP_ROOT`, `CADENCE_APP_NAME`, `CADENCE_APP_PORT`, `CADENCE_APP_HOST`, `CADENCE_APP_CONTRACT`) bound to `127.0.0.1` ports `3110..=3199`.

## 2. `app/app.md` & Explorer `listing:` Metadata

`app/app.md` carries YAML frontmatter identifying the package (`app`, `title`, `version`, `needs.connections` / `needs.capabilities` slots such as `publication_slot` / `required_asset_slot`, `needs.requires.contracts`).

- `needs.requires.contracts` pins exact host contract majors, for example `{ app-views: [1], app-chat: [1], app-storage: [1] }`.
- Optional `listing:` block provides plain-text Explorer metadata (covered by the bundle digest; cannot grant capabilities or declare workflow verbs):
  - Keys: `tagline` (<= 80 chars), `icon` (`assets/*.svg` path), `screenshots` (<= 5 `{ file, caption }`), `category` (`marketing | customers | operations | finance | content | other`), `tags` (<= 5), `publisher`, `about`, `can`, `access_notes`, `changes`, `setup`, `data` (`stores` up to 5 sentences, `personal: true|false`).
  - **Refusals:** unknown keys, HTML, markdown links, control characters, or any `cost`, `price`, or `pricing` key reject the bundle.

## 3. `app/screens/<tag>/screens.json` (`app-screens/v2`)

Each mounted screen declares a manifest under `app/screens/<tag>/screens.json`:

- Bundle bounds: `MAX_SCREENS = 8`, `MAX_ASSETS = 32`, `MAX_MANIFEST_BYTES = 32_768` (`32 KiB`), aggregate bundle size cap `2 MiB`.
- `navigation` block (validated strictly by host):
  - `section_query`: app-owned URL query parameter (`1..=32` chars matching `^[a-z][a-z0-9_-]{0,31}$`). Cannot reuse host-reserved keys (`app`, `install`, `install_id`, `context`, `context_id`, `actor`, `identity`, `auth`, `token`, `key`, `session`, `session_key`, `origin`, `project`, `workspace`, `conversation`, `thread`, `message`, `view`, `appview`, `record`, `record_id`, `segment`, `segment_id`, `screen`, `contract-preview`, `nonce`, `proof`).
  - `sections`: `1..=8` `{ id, label }` objects (unique valid tags for `id`; non-empty `label` <= 40 UTF-16 code units with no control characters).
  - `default_section`: must match one of the declared `sections[].id`.

## 4. `app/app-storage.json` & `app/storage/<tag>.json` (`app-storage/v1`)

Apps that persist structured relational records declare `app/app-storage.json`, flat `app/storage/<tag>.json` migrations, and `needs.requires.contracts: { app-storage: [1] }` in `app.md`.

> **Native vs. Hosted:** Native standalone workspace installs refuse `app-storage` packages; hosted org workspaces validate and provision `app-storage/v1` separately.

- **Descriptor schema (`contract: "app-storage/v1"`):**
  - `app` (`^[a-z0-9][a-z0-9-]{0,31}$`), `schema_version` (`1..=128`), `compatible_schema_versions` (unique integers, must include `schema_version`).
  - `default_table` (identifier or `null`), `record_kind` (`^[a-z][a-z0-9_-]{0,31}$`), optional `record_write_schema` (JSON Schema), optional `record_search_columns` (distinct `text` or `json` columns on `default_table`).
  - `migrations`: contiguous `1..=schema_version` entries `{ version, path: "storage/<tag>.json", sha256: "sha256:<64-hex>" }`.
- **Host-owned base columns (forbidden in column declarations):** `id`, `context_id`, `revision`, `created_at`, `updated_at`.
- **Allowed column types:** `text` (supports `max_length`, `format`, `enum`, `default`), `integer`, `real`, `boolean`, `json` (no `enum`).
- **Allowed migration operations (`op`):**
  - `create_table` (`table`, `columns`, optional `record_bytes`)
  - `add_column` (`table`, `column`; **non-null additive columns require a `default`**)
  - `create_index` (`table`, `name`, `columns`, `unique`)
  - `backfill` (`table`, `column`, `value`, `only_null`)
- **Hard limits (`APP_STORAGE_LIMITS`):**
  - `descriptorBytes`: `32 KiB`, `migrationBytes`: `64 KiB`, `migrationTotalBytes`: `256 KiB`
  - `migrations`: `128`, `tables`: `64`, `columns`: `64`, `indexes`: `32`, `migrationOperations`: `128`, `operations`: `512`
  - `defaultRecordBytes`: `16 KiB`, `recordBytes`: `256 KiB`, `records`: `100,000`, `page`: `100`
- **Production invariants for financial & ledger apps:**
  1. Treat shipped entries in `migrations` as **immutable**—never modify or delete published migration JSON; append a new migration version instead.
  2. Store monetary amounts as **integer minor units** (cents), never floating-point numbers (`real`).
  3. Enforce append-only ledger entries, balanced double-entry invariants, and valid workflow status transitions in application logic and write schemas.

## 5. `app/app-chat.json` (`app-chat/v1`)

Data-only descriptor telling the trusted host `Conversation` frame how to integrate assistant chat:

- Top-level keys (`additionalProperties: false`): `contract: "app-chat/v1"`, `app`, optional `contexts` (`<= 12`, max 3 quick prompts each), `attachments` (`<= 4`), `directives` (`<= 8`, unique `match`, each carrying either a declared text `card` with `<= 2` buttons or `render: "screen:<tag>"`), `subjects` (`<= 8`), and `presentation` (`{ showContext?: boolean }`).
- **Bounds & Forbidden Keys:** whole file `<= 16 KiB`, `<= 1024` JSON nodes, nesting depth `<= 12`; rejects forbidden code/injection keys (`__proto__`, `prototype`, `constructor`, `script`, `scripts`, `code`, `html`, `innerHTML`, `css`, `style`, `javascript`, `eval`, `import`, `module`, `url`, `uri`, `href`, `src`, `link`, `action`).

## 6. Bundled `app/skills/<tag>/` Packages

An app bundle may include scoped agent skills under `app/skills/<tag>/`:

- Each skill contains `skill.json` (`{ schema: 1, name, files }`), `SKILL.md`, flat root files, plus optional `scripts/<stem>.py|sh|js|mjs` and `references/<stem>.md|txt`.
- Caps: max `16` members per skill, `16 KiB` per member, `4 KiB` for `skill.json`, `32 KiB` per skill package.
- Static inventory & token scans run at install/check time; bundled skill scripts carry no execution attestation until explicitly reviewed/approved by the runtime context.
