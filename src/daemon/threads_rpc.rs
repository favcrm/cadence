//! CAD-534: `cadence daemon` threads RPC handlers — moved verbatim from src/daemon.rs; the
//! item→file map is src/daemon/split-map.toml
//! (scripts/split-daemon regenerates it).

use super::*;

impl Shared {
    /// `thread_read` — a page of an agent's thread after `after`,
    /// optionally long-polling up to `wait` seconds (≤ 30) for the next
    /// entry, the way `agent_events` does. Read-only.
    pub(super) fn rpc_thread_read(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Thread cursor must be nonnegative"));
        }
        let limit = optional_i64(params, "limit").unwrap_or(100);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Thread page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        // CAD-328: `tail` (the newest page) or `before` (the page below a
        // seq) read backwards for a chat view; neither waits.
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let before = optional_i64(params, "before");
        if tail || before.is_some() {
            if params.get("after").is_some() || params.get("wait").is_some() {
                return Err(Error::rejected(
                    "Thread read takes either after/wait (forward) or tail/before (backward)",
                ));
            }
            if before.is_some_and(|b| b < 1) {
                return Err(Error::rejected("Thread 'before' must be a positive seq"));
            }
            let thread = self.store.thread(&alias)?;
            let (entries, more) = self.store.thread_entries_before(&alias, before, limit)?;
            let cursor = entries.last().map(|e| e.seq).unwrap_or(0);
            return Ok(json!({
                "alias": alias,
                "thread": thread.as_ref().map(store::Thread::to_json),
                "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
                "cursor": cursor,
                "more_before": more,
            }));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let thread = self.store.thread(&alias)?;
            let entries = self.store.thread_entries(&alias, after, limit)?;
            if !entries.is_empty()
                || Instant::now() >= deadline
                || self.closing.load(Ordering::SeqCst)
            {
                let cursor = entries.last().map(|e| e.seq).unwrap_or(after);
                return Ok(json!({
                    "alias": alias,
                    "thread": thread.as_ref().map(store::Thread::to_json),
                    "entries": entries.iter().map(store::ThreadEntry::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                }));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// `thread_send` — the operator's chat message to an agent: starts
    /// the alias's thread on first use and queues the text exactly like
    /// `agent_send`, recorded as an `operator` entry.
    ///
    /// It instructs an agent, so an agent must never reach it: a
    /// connection the daemon attributes to a pane or managed endpoint is
    /// refused, and so is one whose identity cannot be derived (fail
    /// closed), and a connection tied to no agent must still be provably
    /// the operator (CAD-276's positive proof, CAD-339): the board relays
    /// browser writes from its own process, so the browser's identity is
    /// the board's to establish (CAD-313). Identity-shaped and routing fields are refused rather
    /// than read.
    pub(super) fn rpc_thread_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj
                .keys()
                .find(|k| !matches!(k.as_str(), "alias" | "text" | "message"))
            {
                return Err(Error::rejected(format!(
                    "thread send takes alias, text and message only; field '{field}' \
                     is not accepted"
                )));
            }
        }
        match self.caller_identity(peer_pid) {
            // CAD-339 (review round 1): no agent identity is not enough —
            // a detached child of an agent derives none. The connection
            // must be provably the operator (CAD-276); the board relays
            // browser writes from its own operator process (CAD-313 gap).
            Ok(Caller::NoAgentIdentity) => self.proven_operator("thread send", peer_pid)?,
            Ok(Caller::Agent(v)) => {
                return Err(Error::rejected(format!(
                    "thread send is the operator's chat — this connection is agent \
                     '{}'; agents message each other with `cadence send`",
                    v.agent.alias
                )))
            }
            Err(e) => {
                return Err(Error::rejected(format!(
                    "thread send refused: caller identity underivable — {e}"
                )))
            }
        }
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let mut send = params.clone();
        send["alias"] = json!(alias);
        send["source"] = json!("operator");
        // The thread starts inside the enqueue transaction: a refused
        // message leaves no thread and no `thread_created` event.
        let mut receipt = self.send_as(&send, &|_| Ok(store::Sender::OperatorChat))?;
        receipt["thread"] = self
            .store
            .thread(&alias)?
            .as_ref()
            .map(store::Thread::to_json)
            .unwrap_or(Value::Null);
        Ok(receipt)
    }
}
