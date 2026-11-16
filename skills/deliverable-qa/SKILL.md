---
name: deliverable-qa
description: >
  Independent QA of a finished deliverable in a real browser before a person
  approves, merges or publishes it: a UI change, web or landing page, ad
  creative, social post, deck, PDF or document, or a design mockup. Use when a
  ticket asks for Browser QA, visual QA or a check of something another session
  made. Reports pass or fail with evidence; never approves, publishes, sends or
  spends.
---

# Deliverable QA

A deliverable gets one independent look before a person acts on it. The same
method serves software, business, marketing and design work: open the real
thing in a real browser, check what its kind needs, keep evidence, and file a
plain pass or fail where the next person will read it.

This skill is a task method, not an approval. Whoever owns the deliverable's
workspace rules still decides merges, publishing, sends and spend
(`skills/cadence/SKILL.md`, `skills/company-outputs/SKILL.md`).

## Who runs it

- A session that did not make the deliverable. If you wrote any of it, ask for
  another session instead of checking your own work.
- One QA session at a time per deliverable and revision.

## Before you start

1. Name the exact revision: the PR head's full 40-hex sha, the file's sha256,
   or the artifact's version id. QA of anything else does not count.
2. Read what it was meant to be: the ticket's outcome and acceptance, the
   accepted mockup or brief, and the brand or design tokens if any. A later
   ticket may have changed the brief on purpose: read the linked and later
   tickets, judge against the brief in force, and say which one you used.
3. Serve or open that exact revision, and record how (provenance below). For
   something already published, QA what is actually served. If it is not the
   revision the ticket names, say so and name the commits or tickets that
   changed it.

## Browser

- Default: the host's own headless Chrome. Where `agent-browser` is installed,
  use it in local mode with one session for the whole run
  (`AGENT_BROWSER_FORCE_LOCAL=1 agent-browser --session "$S" ...`), and close it
  at the end. Any other real browser engine works the same way.
- The operator's own browser (for example their Mac over CDP) only when they
  ask to watch. It is shared and single-writer: never close it.
- A public page may use a remote browser; a private or local one never needs to
  be exposed to the internet for QA.

## Provenance (state each separately)

- What was served: the build or file, with the evidence that identifies it
  (a build id read from the running page, a sha256, a version id).
- Where: the URL or path, and what served it (a dev board, a static server,
  a fixture).
- The reviewed revision, whether or not it is what was served.
- Anything you could not prove: write "unverified". A successful build, a
  restart or a familiar-looking URL is not evidence.
- A published page can differ by request: an edge or CDN may inject scripts.
  Hash the origin response (or record its ETag), and note any injected
  difference.

Hosted board surfaces (member chat, the Browser panel, member app screens)
cannot be signed into from an agent session. Use the QA board (CAD-2291) when
it is available, otherwise a loopback stub of the board API, and label every
result from either as **fixture evidence**, not a real-board run. A check on a
real hosted board stays the operator's.

## What to check, by kind

Every kind: the content matches its brief; no console errors or page errors;
screenshots of what you judged, named `NN-<what>-<width>.png`.

- Console tools can miss content-security-policy violations: register a
  `securitypolicyviolation` listener with an init script before the first
  navigation, and report what it catches.
- Before a screenshot, set `scroll-behavior: auto`; a fixed header or footer
  repeated in a full-page shot is the capture, not a defect.
- A check that does not apply (a page with one theme or one language) is "not
  applicable" with the reason, never a pass.
- A third-party destination that fails only in a headless browser (a booking
  or payment page) is "unverified": hand it to a person to open once. Never
  change the browser's identity to get past it.

**UI change (an app screen or board view)**
- Desktop and narrow widths (at least 1440, 1024 and 390), both themes, and
  every locale the product ships.
- Each state the change touches: loading, empty, error, blocked, success.
- Keyboard: every control reachable, visible focus, a sensible order, Esc.
- Nothing overflows (`scrollWidth <= innerWidth`), nothing is cut off or
  overlaps, nothing is alone on a row it should share.
- Canonical navigation and one old deep link: context kept, missing access
  fails safely.
- Every claim in the PR or ticket, one by one.

**Web or landing page**
- The UI checks above that apply, plus: every link resolves, the page title and
  meta description are set and true, images load and have meaningful `alt`
  text, and headings run in order.
- The page still says what it must with JavaScript off, and its own content
  security policy does not block what the page needs.
- Report the page weight (KB transferred and request count) rather than
  judging it by feel.
- Tracking and query parameters: only the ones the brief allows are passed on
  (load the page with an unknown parameter and inspect the outgoing links).
- Forms submit nowhere real during QA.

**Ad creative, social post or image**
- Exact pixel size and format for the placement.
- Text readable at the size it is shown, inside the platform's safe zones.
- Brand colours, fonts and logo as specified; no placeholder text.
- Claims, prices and offers match the approved copy exactly.

**Deck, PDF or document**
- Page count, page size and orientation as asked.
- No overflowing or cut-off text, no missing fonts or images, no placeholder
  text, numbering and links work.
- It opens and reads in a common viewer; an export is legible.

**Design mockup**
- Side by side with the accepted design: layout, spacing, type, colour and
  tokens.
- The states and widths the design specifies.
- Note every difference; say which ones block.

## Filing the result

**A software PR (`ui/**` and other UI changes)**
1. Post the full report as one PR comment, first line
   `Browser QA of PR <n> at <full sha>: PASS` (or `: FAIL`).
2. Only on a pass, post a second, separate comment whose whole body is exactly
   these two lines and nothing else, and never edit it afterwards:

   ```
   Browser QA: pass
   Head: <full 40-hex sha>
   ```

   The merge gate (`scripts/enqueue-reviewed`, `--browser-qa <url>`) counts only
   an unedited comment by a repository member that is exactly those two lines
   for the exact head it merges. A new commit needs a new QA run.
3. A fail is the report only, with steps to reproduce each failure.

**Any other deliverable**
- Post the report as a comment on the deliverable's ticket, with the key
  screenshots attached (`cadence --org <org> issue attach <ID> <file>`).
- First line: `QA of <deliverable> at <revision>: PASS` or `: FAIL`.

**Either way**
- Pass the report on stdin when you cannot write files
  (`cadence --org <org> issue comment <ID> --file -`, `gh pr comment <n> --body-file -`).
- The comment is recorded under the login the command uses. Never pass
  `--author` or otherwise post as someone else.
- QA never changes a ticket's status, owner or acceptance; the person who acts
  on the result does.

**The report** (keep it under a page)
- Provenance.
- A checklist table: check, width or kind, result (pass, fail, unverified,
  not applicable), evidence (screenshot name, marked attached or local; real
  or fixture).
- Failures with steps to reproduce.
- Non-blocking notes.
- What you could not check, and why.

## Where QA stops

- It never approves on a person's behalf, never marks anything ready to merge
  or publish, and never publishes, sends, posts, books or spends.
- It never weakens a check to pass: a state you cannot reach is "unverified",
  not "pass".
- It changes nothing in the deliverable. A fix is the maker's next revision,
  and that revision gets its own QA run.
- Clean up: close the browser session, stop every process you started (by
  process id), and delete scratch files. Evidence lives in what you filed (the
  comment and its attachments), not in local files.
