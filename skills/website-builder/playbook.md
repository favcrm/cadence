# Website builder playbook

Run the scripts from the skill directory (`python3 scripts/<name>.py`). The
`cadence web` commands are run by you, the agent, and only after the matching
owner checkpoint is recorded.

## Step table

| Step | Owner checkpoint | Agent does | Done when |
|---|---|---|---|
| brief | yes | ask the brief questions; record name (slug), goal, pages, contact form | `submit` brief with `owner_confirmed: true` |
| content | no (facts yes) | collect owner text and images into `SRC/index.html` plus assets; use only owner facts | `submit` content with `facts_from_owner: true` |
| build | no | `build.py --src SRC --out dist` | `submit` build with the `out`, `files`, `bytes` it printed |
| review | yes | show the owner `dist/index.html`; record approval or changes | `submit` review with `owner_approved: true` |
| publish | yes | `cadence web prepare dist --name <slug>` freezes and uploads a release; `cadence web publish <release>` returns an approval link; the owner approves in the browser | `submit` publish with `release`, `slug`, `owner_approved: true` |
| verify | yes | `check.py --url <live> --dist dist --record`; then owner confirms | `submit` verify with `owner_accepted: true` |
| done | - | report the live URL and the run id | `next.py` reports `done` |

`check.py` runs only after the owner has approved the release. `submit` refuses
verify unless the recorded check has `status: 200` and `bytes_match: true`.

## Commands

```sh
cadence web prepare dist --name <slug>         # freezes and uploads a release; nothing goes live
cadence web publish <release>                  # returns an approval link; the owner approves it
cadence web status <slug>                      # the live state and any action in flight
cadence web submissions <slug> --form contact  # contact-form submissions, read-only
```

Use `--org <org>` on every hosted command when more than one workspace is selected.

## Known traps

1. **Expired approval.** An approval link is valid for 15 minutes. Once it expires the
   CLI treats that action as terminal, and the old link stays dead. Run
   `cadence web publish <release>` again: it creates a fresh action with a new link.
   Tell the owner the old link no longer works. Re-running while the action is still
   open returns the same link and approves nothing.
2. **Wrong workspace.** If the owner's browser has not selected the workspace, the
   approval read answers 404 and the page says "This approval no longer matches". Ask the
   owner to sign in as an owner of the workspace, select it, and open the link again.
   Check `--org` before you prepare.
3. **`409 asset_conflict`.** Changed bytes at an asset path that an earlier retained release
   still uses are refused. Digest-first names (`build.py`) avoid this: rebuild from the
   current source and prepare again. Never rename an asset by hand.
4. **Form returns 405.** The site has no contact-form route. `check.py` records
   `form: "not_available"`. That is a fact about the site, not a failure: tell the
   owner the form does not work on this site and offer a plain contact link instead.
