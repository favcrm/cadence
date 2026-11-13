# Wiki knowledge: sources, pages, and retrieval

Cadence keeps the wiki in Git-backed text pages and content-addressed raw
attachments. The SQLite full-text index under the daemon state directory is a
disposable search cache. A committed wiki write schedules an asynchronous,
coalesced refresh. Search checks the committed wiki Git tree as a correctness
fallback and repairs an absent or corrupt index before answering. A refresh
reads immutable objects from one captured Git tree and commits rows plus the
indexed tree revision atomically. Search results include the page path,
line, heading, revision, source path, excerpt, and rank. A caller must fetch the
page before using an excerpt as evidence. The existing wiki path permissions
apply to both search results and fetches; this feature does not change them.

## Content model

Use three durable layers, following the LLM wiki pattern:

1. **Raw source.** Keep the original PDF or other attachment as a wiki blob.
   Do not edit it in place. A replacement creates a new content hash and a
   versioned pointer. A Markdown or TXT upload is already readable source text
   and is stored directly as a Git page.
2. **Readable source text.** A PDF upload writes a neighboring
   `<name>.pdf.extracted.md` page with `source_path`, `source_sha256`, and
   `extraction_status` frontmatter. `pdftotext` is bounded to 20 seconds and
   16 MiB of output. A converter failure or scanned PDF retains the raw blob
   and records `failed` or `empty` without keeping text from an older PDF.
   OCR is not provided. The index ignores a generated page when its source
   pointer is missing or names different bytes.
3. **Curated wiki.** An authorized agent, board member or operator writes concise Markdown
   pages that summarize one durable topic, link to the source pages, name
   uncertainty, and update existing pages when evidence changes. For company
   knowledge, board members and the operator may create, edit, move and delete
   pages under `global/` and `projects/<key>/`. Members also control their own
   `users/<handle>/`. Other users' private pages, agent-owned areas and the
   namespace roots remain protected. The master may draft in
   `agents/master/knowledge/`; its current permission does not let it publish
   into `global/`.

Start each curated page with the question it answers and a short answer. Link
each claim to its source path and revision when a revision matters. Keep a
small `index.md` in each active folder as a human and agent navigation map;
the search database is a derived candidate finder, not that map. On a new
source, search for existing topic pages, update the relevant ones, and record
contradictions and open questions. Periodically check broken source links,
orphan pages, and claims whose source has changed. Do not automatically turn
an uploaded document into accepted operational guidance.

### Board navigation and recovery

The Wiki root lists the fixed namespaces; it is not itself a place to create
pages. Open a writable folder such as `global/`, a project folder, or your
personal folder. Native boards identify that folder by handle; hosted boards
use the verified member subject. A read-only message on one folder does not
mean every folder is read-only.

An empty project Context offers to create its wiki folder. Search filters use
the available file type, and a hosted result that matched only some search
terms says so. Hosted attachment filenames remain searchable even when their
text cannot be extracted. An upload's extraction status is a receipt, not a
live progress feed; an unconfirmed result does not establish that extraction
has finished. Hosted history identifies your edits as yours and uses a neutral
contributor label for other, unresolved subjects.

Removing an item returns its exact `.trash/...` landing. An operator can open
Trash in the board, inspect the recorded entries, and restore to the original
path or another available destination. Trash is not visible to members or
agents. The browser does not guess a restore source when a native landing's
original path cannot be proved from its tracker history; it reports when its
bounded history search stops before finding proof. Keep the original receipt:
an operator can restore that exact source with
`cadence wiki mv <landing> <destination>` after the server's safety checks.
Older servers may not support the trash browser.

Hosted restores with more than 200 distinct stored attachments are refused
until an enforceable storage-custody policy exists. Smaller hosted restores
check that the referenced objects exist immediately before the move; an
independent, privileged deletion after that check can still race the restore.
The check is not a backup or proof of a tenant bucket lock. An older revision
whose original text was not retained cannot be reconstructed from its history
entry.

## Upload and update

The board Wiki Upload pane sends a complete destination path for every file.
Markdown and TXT uploads create or update a text page. Other uploads use the
blob store; PDF text extraction runs after the blob commits and the response
includes an `extraction` status. The board warns when a PDF was saved but its
text could not be indexed.

On hosted boards, editable Markdown and TXT uploads are limited to 256 KiB;
larger text uploads are refused before storage rather than becoming attachments.

An operator can use the CLI:

```sh
cadence wiki put global/onboarding.md --file onboarding.md --if-rev none
cadence wiki put global/handbook.pdf --blob --file handbook.pdf --if-rev none
cadence wiki search onboarding --path global --json
cadence wiki cat global/onboarding.md --meta
cadence wiki cat global/onboarding.md
cadence wiki cat global/onboarding.md --offset 0 --bytes 4096
cadence wiki cat global/onboarding.md --offset 4096 --rev fnv1a:0123456789abcdef
```

`cat --meta` returns the whole text page and its revision as one JSON line.
A page whose line would exceed the native tool's 50000-byte line bound is
read with `--offset` and optional `--bytes`. The reply is one compact JSON
line: canonical `path`, original `rev` (the whole page, not the slice),
`range` (`start` inclusive, `end` exclusive, `total` bytes of the original)
and the readable UTF-8 `text`. `text` is exactly the original bytes
`[start, end)`. When `--bytes` is omitted, `end` is the last byte actually
returned under the line bound, not a longer requested length. An explicit
range that would pass the bound is refused rather than returned short.
Omit `--rev` on the first range and pass that returned revision on every
later range. A different revision is refused and no body is returned, so
ranges are never combined across versions. An offset or end inside a UTF-8
character is refused and no partial character is returned. An unauthorized
caller gets the same refusal as `cat`, with no body. The read uses the same
caller allowlist as `cat`; it does not copy the page or change the text cap.

A blob page is not text. `cadence wiki cat <path>` streams the original
bytes: locally from the content-addressed object, and on a remote org by
repeating the bounded read with the pointer `rev` pinned until `range.total`
is covered. Each remote range is checked for `start`, `end`, `total`, `rev`,
and `sha256`. The decoded bytes go to a private spool, and stdout is written
only after that spool's SHA-256 matches the pointer. A server that returns
only pointer metadata, or a short, stale, or hash-mismatched range, fails
and writes nothing. `--meta` still prints the pointer and does not fetch
bytes. `--offset`, `--bytes`, and `--rev` on a blob print one compact JSON
line: the same pointer fields and `range` as a text range, plus `data_base64`
(standard base64 of the original bytes `[start, end)`). That line stays under
the native tool bound. An explicit range that would exceed the bound is
refused. An omitted `--bytes` returns the longest fitting slice. Empty blobs
return an empty slice. Text pages do not use `data_base64`.
Remote `cat` refuses blobs larger than 100 MiB before fetching byte ranges.
Discovery and continuation reads share one `--wake-timeout` deadline. Expiry
keeps the existing retryable `busy`/`waking` exit 75 and releases no blob bytes.

`--if-rev none` is create-only. For an update, read the current revision with
`cat --meta`, then pass that value to `put --if-rev <rev>`; a conflict means
another writer won and the caller must reread. The board editor does this
automatically. A PDF replacement updates the generated text page; moving or
removing a PDF cleans up its generated page. Moving a folder rewrites the
`source_path` on generated pages inside it; an incomplete rewrite is excluded
from search until corrected. A failed conversion remains visible as an
extraction status, with the raw source still available.

### Operator file import (`wiki upload`)

`cadence wiki upload <file> <path> [--overwrite]` imports one local file into
the remote knowledge wiki for retrieval. It is an explicit, user-invoked import:
nothing uploads automatically, and chat attachments never use this path (they
stay in the user's thread workspace). Limits: 15 MiB per file, regular files
only (symlinks and FIFOs are refused), no chunking yet.

Without `--overwrite`, the parent folder is listed first and an existing name is
refused. That check is not atomic: a file created between the listing and the
upload can still be replaced. Use `--overwrite` only for files you control.

### Recursive copy (`wiki cp`)

`cadence wiki cp SOURCE DEST --recursive [--update] [--resume JOB_ID] [--wait-index]`
copies a tree. Exactly one operand is `wiki:<canonical-path>`; the other is
local. `--org` selects the remote workspace once. Local and remote copies use
the same transfer operations (`begin`, `chunk`, `apply`, `finish`, `status`,
`export`). A directory source requires `--recursive`. Empty directories and
empty files are kept. `.md`, `.markdown`, and `.txt` are text pages; other
regular files are attachments.

A hosted workspace board serves these transfer operations since its 2026-11-12
release (AOS-566). On a hosted board:

- Every operation is two requests (authorize, then call), and the whole command
  has one deadline, `--wake-timeout` (120 seconds by default). Each file costs
  at least two operations, so a copy of more than a few dozen files can run out
  of time: pass a longer `--wake-timeout`, or let it stop and continue the same
  job with `--resume <job>`. A resume checks every entry again, so a very large
  copy is better split into several smaller ones.
- The board sets its own size limits for `wiki cp`, which are lower than this
  client's. A file over the limit refuses the whole copy before anything is
  staged; send a large binary file on its own with `wiki upload`.
- A refusal names the board's code and what to do, for example
  `destination_changed` (copy again with `--update`) or `payload_too_large`.
  An export reports a file the board refused in its JSON failure list (exit 1)
  and stops there; the board's code is in that entry's text.
- A board released before then answers `not_implemented` and copies nothing;
  there, write each text page with `wiki put` and send each binary file with
  `wiki upload`.

The command hashes the source and refuses symlinks, special files, and
internal paths before it mutates the wiki. It prints the job id and digest
before entry writes. Repeat the same source and `--resume JOB_ID` after an
interruption. Resume reuses the admitted manifest and its destination revisions;
it does not read those revisions again. The client chooses the job id before
`begin`; if its response is lost or unusable, the error names that same id.
Inspect or resume that job rather than starting a fresh copy blindly.
An empty directory is admitted with no entries, and finish creates that root.
`--update` pins the destination's current revisions and replaces changed
bytes. Without it, identical content can be skipped, but changed existing
pages are create-only. A partial job exits nonzero and names the job id.
When a chunk, apply or finish stops partway, the exit code says what is known
about that one request. Entries applied earlier in the same job stay applied
in every case.

- **3** — a hosted board refused the request with one of its own transfer
  codes, and the error keeps that code. That request changed nothing. Follow
  the message: resume with `--resume <job>`, or copy again without `--resume`
  when the job is gone.
- **75** — the command deadline passed before the request was sent, or the
  board's gateway answered that the board was still waking and did not forward
  it. That request changed nothing. Run the same copy again with
  `--resume <job>`, with a larger `--wake-timeout` if the board was still
  waking; a plain rerun starts a new job instead.
- **1** — the outcome is not confirmed: a reply was lost, a local daemon refused
  (its refusal can follow a publish), or the client cannot prove the request
  was never sent. It may have landed. Run the same copy with `--resume <job>`
  before repeating anything; the job shows what was recorded.

A local source problem found while sending a chunk, such as a file that changed
or shrank after the manifest was pinned or could not be read, exits 3: that run
did not apply the file and did not finish the job; copy again without
`--resume`. The same holds when the board's job holds more bytes for a file than
its pinned size, which resuming cannot fix. A chunk answer the client cannot
read exits 1.

`--wait-index` polls that job's own status until the index is covered, the job
fails, or the command deadline
passes. It does not refresh the global index.

Matching working-tree bytes alone are not proof of publication. If the desired
revision cannot be proved in Git, the entry remains `unresolved`, retains its
staged input, and cannot finish. The command reports the job instead of silently
committing an unproved file. After an authorized publication resolves the target,
resume the same job; an already committed matching revision is skipped without
another commit.
An interrupted tracker write may also leave staged paths fenced against normal
Wiki writes. An authorized operator must explicitly resolve those exact paths
using the tracker's documented recovery procedure before resuming. Transfer
resume does not repair the Git index or replay an uncertain publication.

The reverse direction exports only the caller's tree, pins each file's
revision and hash, stages bytes privately, and publishes relative to held
no-follow directory handles. Default publication is atomic and create-only;
`--update` explicitly permits atomic replacement. A concurrently planted
destination file is not overwritten without `--update`.

Storage, extraction and search readiness are separate receipts. A PDF whose
extraction fails retains its downloadable original but exits nonzero; failed
or empty extraction is never reported as index-covered. Ordinary empty text
files remain valid copies. Finishing a job coalesces index work; startup
rechecks completed-job cache freshness without republishing source files.
A completed empty-root job becomes `superseded` if its directory is removed
or moved; status never recreates the root. Public-board jobs bind to the
verified subject and realm, not a particular session token, so renewal keeps
custody while another subject or an unbound local caller cannot inherit it.
Private staged inputs are discarded only after terminal outcomes are durable;
unresolved publication retains them for reconciliation.

Coordination-profile portable export omits transfer jobs and private staged
inputs; it is not a backup of in-flight transfer custody. Export leaves the
source jobs and stages unchanged. An inactive portable restore cannot resume
those source jobs: admit a fresh transfer from the original inputs instead.

Native admission caps are 256 entries, a 256 KiB manifest, 100 MiB per file
(also subject to configured wiki caps), 256 MiB total, and 16 KiB decoded
chunks. Status and directory metadata are byte-bounded pages, at most 32
entries each. Hosted forwarding retains its stricter **64 KiB whole-request**
cap: a manifest accepted locally may be refused before hosted admission.
Use a matching native/hosted release; local proof does not authorize an image
repin, schema migration, or production write.

## Index health and repair

An operator can inspect the derived index without triggering a rebuild:

```sh
cadence wiki index status
cadence wiki index refresh
```

Status reports the current and indexed Git tree IDs, schema, whether the index
is current, stale or error reasons, page/chunk/skipped counts, last successful
refresh time and duration, and the latest refresh error. `refresh` forces a
complete rebuild from the committed tree, including when the index already
reports current. A normal wiki write succeeds once committed even if its
background refresh fails. The next search retries synchronously and returns an
error if it cannot produce a current index; it does not return stale hits.
Corrupt SQLite files are retained beside the new index as
`wiki-search.sqlite3.corrupt-*` for diagnosis. Status and repair require an
operator connection; agents cannot invoke them.

Back up the wiki's Git repository and the wiki `.blobs/` directory together.
Git holds the text pages, blob pointers, and extraction provenance; `.blobs/`
holds the bytes named by those pointers. Restore both before restarting the
daemon, then run `cadence wiki index refresh`. The SQLite file under the daemon
state directory is derived and is not needed for restoration. Retain Git
history and the blob bytes referenced by its pointer history when planning
backup retention or eventual garbage collection.

## Agent retrieval

The master uses `cadence wiki search <terms> --json`, reads selected pages
with `cadence wiki cat <path>`, and cites their paths. A page that does not
fit the native tool line bound is read with `cat --offset/--bytes/--rev`
instead of a shell slice. Registered native agents use the same CLI under their connection-derived identity. A connected board
client uses `GET /api/wiki/search?q=<terms>&path=<scope>` and then
`GET /api/wiki/file?path=<selected page>`. Text pages return JSON with the
full text and revision. The same route accepts optional `offset`, `bytes`,
and `rev` query parameters and returns the bounded range read above; a bad
integer or empty revision is refused before the page is read. Omit them to
keep the whole-page reply. A blob range query returns the compact JSON
object above (`data_base64` included) instead of the file bytes. With no
range query, the board session route still streams the blob, and a verified
`wikienv_` read returns pointer metadata. A verified `wikienv_` read of that
same path is the hosted hop: it forwards those three query parameters only
after the envelope's signed prefix check. A path outside that prefix is
refused before the daemon reads the page, and the prefix ceiling is unchanged.
The board's existing session or attributed-agent identity is required; the
permission-prompt MCP server is not a wiki API.

Search uses SQLite FTS5 term matching and BM25 ranking over titles, headings,
and approximately 4 KiB text chunks. It is lexical retrieval, so the agent
should try domain terms and synonyms, inspect more than one hit when a question
spans topics, and follow source links in curated pages. Graphify can later
derive relationship views from these source-backed pages for cross-document
exploration. Its inferred edges should carry provenance and must not replace
the canonical text or the direct source fetch. Reviewed project memory remains
a separate, short lesson workflow: use `cadence memory propose`, independent
reviews, and acceptance when a fact should be injected into future dispatches.
