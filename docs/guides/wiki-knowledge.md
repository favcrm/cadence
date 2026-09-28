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
3. **Curated wiki.** An authorized agent or operator writes concise Markdown
   pages that summarize one durable topic, link to the source pages, name
   uncertainty, and update existing pages when evidence changes. For company
   knowledge, the operator publishes under `global/`. The master may draft in
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

## Upload and update

The board Wiki Upload pane sends a complete destination path for every file.
Markdown and TXT uploads create or update a text page. Other uploads use the
blob store; PDF text extraction runs after the blob commits and the response
includes an `extraction` status. The board warns when a PDF was saved but its
text could not be indexed.

An operator can use the CLI:

```sh
cadence wiki put global/onboarding.md --file onboarding.md --if-rev none
cadence wiki put global/handbook.pdf --blob --file handbook.pdf --if-rev none
cadence wiki search onboarding --path global --json
cadence wiki cat global/onboarding.md --meta
cadence wiki cat global/onboarding.md
```

`--if-rev none` is create-only. For an update, read the current revision with
`cat --meta`, then pass that value to `put --if-rev <rev>`; a conflict means
another writer won and the caller must reread. The board editor does this
automatically. A PDF replacement updates the generated text page; moving or
removing a PDF cleans up its generated page. Moving a folder rewrites the
`source_path` on generated pages inside it; an incomplete rewrite is excluded
from search until corrected. A failed conversion remains visible as an
extraction status, with the raw source still available.

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
with `cadence wiki cat <path>`, and cites their paths. Registered native agents
use the same CLI under their connection-derived identity. A connected board
client uses `GET /api/wiki/search?q=<terms>&path=<scope>` and then
`GET /api/wiki/file?path=<selected page>`. Text pages return JSON with the
full text and revision. The board's existing session or attributed-agent
identity is required; the permission-prompt MCP server is not a wiki API.

Search uses SQLite FTS5 term matching and BM25 ranking over titles, headings,
and approximately 4 KiB text chunks. It is lexical retrieval, so the agent
should try domain terms and synonyms, inspect more than one hit when a question
spans topics, and follow source links in curated pages. Graphify can later
derive relationship views from these source-backed pages for cross-document
exploration. Its inferred edges should carry provenance and must not replace
the canonical text or the direct source fetch. Reviewed project memory remains
a separate, short lesson workflow: use `cadence memory propose`, independent
reviews, and acceptance when a fact should be injected into future dispatches.
