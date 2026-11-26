# Contracts, Manifests, and Storage (`app-storage/v1`, `app-chat/v1`, `screens.json`)

Always inspect the target repository's existing manifests and pinned contract versions before adding or editing files.

## 1. Root `package.json` `cadence` Block

Detached app repositories separate the outer repo root (`.`, containing `package.json` for `cadence app dev --package .`) from the bundle root (`./app`, passed to `cadence app check ./app --json`):

- Required/standard fields in root `package.json` `cadence`: `cadence.app` (app identifier), `cadence.dev` (local dev command), `cadence.check` (verification command).
- **Command grammar rule:** Commands in `cadence.dev` must be a single command: newlines (`\n`/`\r`), unescaped backticks, and `$()` command substitution are rejected everywhere (even inside quotes), and unquoted `;`, `&`, `|`, `<`, `>`, `(`, and `)` are rejected; put multiple steps in a script file. `cadence.check` runs via `sh -c`.
- `cadence app dev --package .` clears the environment (`env_clear()`), copies only allowlisted host vars (`PATH`, `HOME`, `LANG`, `TERM`, `PNPM_HOME`, `XDG_CACHE_HOME`, `TMPDIR`), and injects `CADENCE_APP_ROOT`, `CADENCE_APP_NAME`, `CADENCE_APP_PORT`, `CADENCE_APP_HOST`, and `CADENCE_APP_CONTRACT` bound to `127.0.0.1` ports `3110..=3199`.

## 2. `./app/app.md` & Explorer `listing:` Metadata

`./app/app.md` (bundle-relative path `app.md`) carries YAML frontmatter identifying the package (`app`, `title`, `version`, `needs.connections` / `needs.capabilities` slots such as `publication` or `image`, `needs.requires.contracts`). Inside `workflows/<tag>.md` frontmatter, `publication_slot: <slot>` references a slot declared in `app.md` `needs.capabilities` (and is forbidden from appearing in `capability_slots`), whereas `required_asset_slot: <slot>` must name a slot declared in that workflow's own `capability_slots: [...]` list (`1..=8` static slot names) — and every slot in `capability_slots` must also be declared in `app.md` `needs.capabilities` with `effect: read` or `draft` (`app.md` parse/install enforces `<= 8` total `needs.capabilities` slots and at most `1` `"send"` slot in `src/issue/app.rs`, while run creation enforces the `capability_slots` `read`/`draft` constraint in `src/daemon/app_runs_rpc.rs`). Never declare `publication_slot` or `required_asset_slot` itself as a literal slot name inside `needs.capabilities`.

- `needs.requires.contracts` pins exact host contract majors, for example `{ app-views: [1], app-chat: [1], app-storage: [1] }`.
- Optional `listing:` block provides plain-text Explorer metadata (covered by the bundle digest; cannot grant capabilities or declare workflow verbs):
  - Keys: `tagline` (<= 80 chars), `icon` (`assets/*.svg` path), `screenshots` (<= 5 `{ file, caption }`), `category` (`marketing | customers | operations | finance | content | other`), `tags` (<= 5), `publisher`, `about`, `can`, `access_notes`, `changes`, `setup`, `data` (`stores` up to 5 sentences, `personal: true|false`).
  - **Refusals:** unknown keys, HTML, markdown links, control characters, or any `cost`, `price`, or `pricing` key reject the bundle.

## 3. `./app/screens/<tag>/screens.json` (`app-screens/v2`)

Each mounted screen declares a manifest under bundle-relative `screens/<tag>/screens.json` (`./app/screens/<tag>/screens.json` in a detached app repo):

- Bundle bounds: `MAX_SCREENS = 8`, `MAX_ASSETS = 32`, `MAX_MANIFEST_BYTES = 32_768` (`32 KiB`), aggregate bundle size cap `2 MiB`.
- `navigation` block (validated strictly by host):
  - `section_query`: app-owned URL query parameter (`1..=32` chars matching `^[a-z][a-z0-9_-]{0,31}$`). Cannot reuse host-reserved keys (`app`, `install`, `install_id`, `context`, `context_id`, `ctx` [TS], `actor`, `identity`, `auth`, `token`, `key`, `session`, `session_key`, `origin`, `project`, `workspace`, `conversation`, `thread`, `message`, `view`, `appview`, `record`, `record_id`, `segment`, `segment_id`, `screen`, `contract-preview`, `nonce`, `proof` [Rust], `generation` [TS], `digest` [TS] — avoid all 31 keys across Rust/UI layers).
  - `sections`: `1..=8` `{ id, label }` objects (unique valid tags for `id`; non-empty `label` <= 40 UTF-16 code units with no control characters).
  - `default_section`: must match one of the declared `sections[].id`.

## 4. `./app/app-storage.json` & `./app/storage/<tag>.json` (`app-storage/v1`)

Apps that persist structured relational records declare bundle-relative `app-storage.json`, flat `storage/<tag>.json` migrations, and `needs.requires.contracts: { app-storage: [1] }` in `app.md`.

> **Native CLI vs. Hosted Authority:** Native `cadence app check <bundle-dir>` enforces `CompatibilityCheck::RequireNative` and **refuses** (exit `3`) any package declaring `needs.requires.contracts.app-storage: [1]` (`"app requires host contract 'app-storage' major 1 — this host supports none (unknown contract)"`), and native workspace installs also refuse `app-storage` packages. Run `cadence app inspect <bundle-dir> --json` locally to validate bundle integrity and `app-storage.json` / `storage/<tag>.json` path-presence pairing, and validate the authoritative `app-storage/v1` descriptor, table, column, and migration schema against the hosted target repo (`agenticos-v2` `packages/contracts/src/app-storage.ts`) + hosted `cadence --org <slug> app catalog release-check`.

- **Descriptor schema (`contract: "app-storage/v1"`):**
  - `app` (`^[a-z0-9][a-z0-9-]{0,31}$`), `schema_version` (`1..=128`), `compatible_schema_versions` (unique integers, must include `schema_version`).
  - `default_table` (identifier or `null`), `record_kind` (`^[a-z][a-z0-9_-]{0,31}$`), optional `record_write_schema` (JSON Schema), optional `record_search_columns` (distinct columns; requires `default_table !== null`), optional `tables` (requires `default_table !== null`).
  - `migrations`: contiguous `1..=schema_version` entries `{ version, path: "storage/<tag>.json", sha256: "sha256:<64-hex>" }`.
- **Host-owned base columns (forbidden in column declarations):** `id`, `context_id`, `revision`, `created_at`, `updated_at`.
- **Column declarations (`appStorageColumnSchema`):** Every column requires `name`, `type`, and `nullable` (`boolean`), plus optional constraints: `text` (`max_length`, `format`, `enum`, `default`, `json_schema`), `integer` (`enum`, `default`, `json_schema`), `real` (`enum`, `default`, `json_schema`), `boolean` (`enum`, `default`, `json_schema`), `json` (`default`, `json_schema`, no `enum`).
- **Table & row-policy options (`tables` descriptor):** Non-default table entries accept `search_columns`, `write_schema`, `append_only`, `read_role` (`"owner"`), `balanced` (`{ amount, by }`), `lock`, and `row_owner`. The `default_table` entry in `tables` may only declare `append_only`, `read_role`, `balanced`, `lock`, and `row_owner` (its search columns and write schema live at top-level `record_search_columns` and `record_write_schema`).
- **Allowed migration operations (`op`):**
  - `create_table` (`table`, `columns`, optional `record_bytes`)
  - `add_column` (`table`, `column`; **non-null additive columns require a `default`**)
  - `create_index` (`table`, `name`, `columns`, `unique`)
  - `backfill` (`table`, `column`, `value`, `only_null`)
- **Runtime limits (enforced by the hosted target `app-storage/v1` runtime):**
  - `descriptorBytes`: `32 KiB`, `migrationBytes`: `64 KiB`, `migrationTotalBytes`: `256 KiB`
  - `migrations`: `128`, `tables`: `64`, `columns`: `64`, `indexes`: `32`, `migrationOperations`: `128`, `operations`: `512`
  - `defaultRecordBytes`: `16 KiB`, `recordBytes`: `256 KiB`, `records`: `100,000`, `page`: `100`
- **Production invariants for financial & ledger apps:**
  1. Treat shipped entries in `migrations` as **immutable**—never modify or delete published migration JSON; append a new migration version instead.
  2. Store monetary amounts as **integer minor units** (cents), never floating-point numbers (`real`).
  3. Enforce append-only ledger entries, balanced double-entry invariants, and valid workflow status transitions in application logic and write schemas.

## 5. `./app/app-chat.json` (`app-chat/v1`)

Data-only descriptor (bundle-relative `app-chat.json`) telling the trusted host `Conversation` frame how to integrate assistant chat:

- Top-level keys (`additionalProperties: false`): `contract: "app-chat/v1"`, `app`, optional `contexts` (`<= 12`, max 3 quick prompts each), `attachments` (`<= 4`), `directives` (`<= 8`, unique `match`, each carrying either a declared text `card` with `<= 2` buttons or `render: "screen:<tag>"`), `subjects` (`<= 8`), and `presentation` (`{ showContext?: boolean }`).
- **Bounds & Forbidden Keys:** whole file `<= 16 KiB`, `<= 1024` JSON nodes, nesting depth `<= 12`; rejects 53 forbidden code/injection/callback/URL keys defined in `contracts/app-chat/v1/app-chat.schema.json` `$defs.safeObject` (such as `__proto__`, `prototype`, `constructor`, `script`, `scripts`, `code`, `html`, `innerHTML`, `css`, `style`, `javascript`, `eval`, `import`, `module`, `url`, `uri`, `href`, `src`, `link`, `action`).

## 6. Bundled `./app/skills/<tag>/` Packages

An app bundle may include scoped agent skills under bundle-relative `skills/<tag>/`:

- Each skill contains `skill.json` (`{ schema: 1, name, files }`), `SKILL.md`, flat root files matching `<stem>.(md|txt|json|yaml|yml|csv|css|svg)`, plus optional `scripts/<stem>.py|sh|js|mjs` and `references/<stem>.md|txt`.
- Caps: max `16` members per skill, `16 KiB` per member, `4 KiB` for `skill.json`, `32 KiB` per skill package.
- **Script execution is agent-side and unattested:** static inventory & undeclared-reference token scans run on native install/check, native runs freeze declared bytes and digests into `snapshot.workflow.steps[].skill` without host-side execution or sandboxing beyond the worker's own confinement, and hosted boards (`app_skill`) list `scripts/` by name only without returning or running them. Approving a bundle or run approves the frozen package material—never present a package digest or runtime approval as proof that a script ran or ran as written.

## 7. Shared Workspace Brand Kit (`global/brands/<slug>/`)

All brand-aware Cadence apps read and maintain brand context from Workspace Wiki under `global/brands/<slug>/` (`default` for the primary workspace brand; slug format `^[a-z0-9][a-z0-9-]{0,47}$`):

- **Directory layout (`global/brands/<slug>/`):**
  - `brand.md` — identity, positioning, target audience, locale, tone pills, CTA style.
  - `guardrails.md` — protected terms, forbidden claims, required price/legal disclaimers.
  - `examples.md` — few-shot exemplar captions/copy with notes on why they work.
  - `visual.md` — palette hex swatches (`Background`, `Accent`, `Surface / Type`, `Shadow`), typography feel, art direction.
  - `assets/` — visual reference images/logos referenced by media generators.
  - `global/brands/_map.json` (optional) — maps connected destinations/channels to specific brand slugs.
- **Bounds & host contract:** Keep enumeration bounded (`MAX_BRANDS = 32`, `MAX_ASSETS = 24`, `MAX_FILE_CHARS = 8_192`). Persist selected `brand_slug` in context defaults (`""` for default workspace brand) with optional per-draft overrides.
- **ACL & injection rule:** Reads use shared workspace wiki scope; writes go through operator/member Workspace Wiki ACLs under `global/`. Sanitize extracted markdown/asset metadata before prompt injection and quote it strictly as reference data.
