//! CAD-1129 acceptance checks (A1–A9) — **to be written by the
//! reviewer, not the implementer.**
//!
//! Per the ticket's "Gates and security work" rule, the checks that
//! prove the bad cases are refused are authored by someone other than
//! the implementer and the implementer may not edit them. This module
//! is the marked home: a reviewer or the ticket author fills each test
//! below against the real guard, the implementer only runs it.
//!
//! Acceptance (from the ticket / build plan):
//!
//! - **A1** — `app_catalog_list` and `app_catalog_show` answer the two
//!   built-ins with their listing, access sentences and trust chip; a
//!   member gets the same rows minus `digest`/`request_count`, and a
//!   forged `member_as` never resolves a member session.
//! - **A2** — `app_home` reads live installs only; a soft-removed
//!   install reads `removed` and is absent from Open and the catalog
//!   "installed" state.
//! - **A3** — `app_workspace_install_entry` refuses a second install
//!   of one app, and records `Source::Builtin` with the shipped digest.
//! - **A4** — `app_catalog_git_check` refuses a credential-bearing,
//!   non-HTTPS, IP-literal, loopback, query or fragment URL before any
//!   network step; a valid check resolves the ref to a commit and
//!   answers an "unverified" card.
//! - **A5** — `app_workspace_update_check` checks the record's stored
//!   `source` upstream only — the body carries no `source`/`url`; a
//!   caller-named upstream is refused.
//! - **A6** — `app_workspace_remove` marks the record, revokes consent
//!   (`app_install_revoke`), cancels queued publishes and refuses run
//!   creation; a stale generation or digest refuses before any write.
//! - **A7** — `app_workspace_restore` clears the mark and re-consents
//!   the same digest; after `restore_after` it refuses.
//! - **A8** — `app_favorites_*` bind to the re-proved `member_as`
//!   (never a client field); `app_favorites_put` refuses a removed or
//!   unknown install id; `install_ids` is bounded.
//! - **A9** — The bundle validator refuses a `listing` carrying
//!   `cost`/`price`/`pricing` or an `assets/` member that is not a
//!   flat `*.svg` — at parse, never at install.

#[cfg(test)]
mod tests {
    // The reviewer authors each check here; this file deliberately
    // ships no passing implementation.
}
