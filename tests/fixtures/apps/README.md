# tests/fixtures/apps

Historical host-contract test fixtures.

`crm/` and `social-content/` are frozen snapshots of the retired root
`workspace-apps/` bundles. They exist ONLY so daemon and UI tests can
exercise the host contract (bundle layout, `app.md` grammar,
`app-chat.json`, workflows, rubrics, assets) against known input.

They are NOT distributable apps, NOT installed at runtime, and NOT the
canonical source for any app. Do not edit them to add features; if the
host contract changes, update the fixtures to match the contract, not
the other way around.
