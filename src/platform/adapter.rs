//! CAD-506 / ADR 0006 §5.1, §5.2: the platform-adapter seam the effect
//! gate executes through. An adapter knows one platform's API: its
//! reviewed tool table (`{tool → {effect, scopes, label}}`), the
//! manifest version the platform currently reports, and the calls —
//! execute and read-back — the proxy drives.
//!
//! The gate owns classification (C1–C3), grants and the pending-effect
//! lifecycle; the adapter owns the platform's terms. A call reaches
//! `execute` only inside the agent's grant and, for a send, only after
//! the durable operator press — custody bytes cross the seam at call
//! time and never return.
//!
//! The shared-fixture vocabulary types ([`ToolTable`], [`Verified`])
//! live in [`crate::contract_fixture`]: they are the contract both
//! repos implement (CAD-505), not test doubles — the fake platform is
//! the same file's executable copy.
//!
//! Adapters register per platform name in
//! [`crate::daemon::ServeOptions::platforms`]; CAD-367 lands
//! Cloudflare's, CAD-501 AgenticOS's. A platform with no registered
//! adapter refuses every call — the gate fails closed.

use serde_json::Value;

use crate::contract_fixture::{ToolTable, Verified};

/// One platform's adapter — the proxy's outward leg.
pub trait PlatformAdapter: Send + Sync {
    /// The adapter's declared tool table, reviewed like code and pinned
    /// to the manifest version it was reviewed against (§5.2).
    fn table(&self) -> &ToolTable;

    /// The manifest version the *platform* reports now — the platform
    /// side of the pin. `None` reports nothing, so every call gates as
    /// `send` (a call argument can never assert the match).
    fn reported_manifest_version(&self) -> Option<String>;

    /// The rendered, bounded preview the press reviews — what the
    /// platform will do, in the platform's terms, on `account`
    /// (§5.4 `preview`).
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String;

    /// Perform the call against the platform. `credential` is the
    /// enrolled custody bytes — read inside the gate, attached here,
    /// never returned. `idempotency_key` derives from the `effect_id`
    /// of the staged send (C9); a repeated key must return the recorded
    /// outcome without executing again. `expected_hash` is the approved
    /// input's content hash where the platform supports one.
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        idempotency_key: &str,
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String>;

    /// §5.4 step 6 read-back: does platform state match the approved
    /// input? `Verified::Unknown` where the platform offers none.
    fn read_back(&self, tool: &str, input: &Value) -> Verified;

    /// The content hash (`sha256:<hex>`) of a reviewed source artifact
    /// the platform holds — `None` when it holds no such artifact. A
    /// send pins it as `source_hash`; Execute re-verifies it before
    /// firing (§5.4 step 3). `agent` is the row's proven requester: an
    /// adapter whose artifacts name caller-owned scope binds the name
    /// to it — `local`'s attachment descriptor may pin only that
    /// agent's own worktree (CAD-553).
    fn source_hash(&self, agent: &str, source: &str) -> Option<String>;

    /// The reviewed artifact a send will release, named by the
    /// adapter from the proven caller and its input when `input.source`
    /// does not name one (CAD-553: `local` names the declared
    /// attachment set it will read under the requester's worktree).
    /// `None` implies nothing — the send stages unpinned. An implied
    /// name rides the same `source_hash` pin and `source_changed`
    /// close as a caller-declared one; the gate knows no adapter's
    /// name format.
    fn implied_source(&self, _agent: &str, _tool: &str, _input: &Value) -> Option<String> {
        None
    }
}
