//! CAD-886: `agent_wait` — block until an agent reports, idles or needs
//! attention.
//!
//! The wait happens on the daemon side: one waiter on the existing
//! daemon-wide `changed` Notify, re-checked in 250 ms quanta (the same
//! primitive `agent_ask` and `agent_events --wait` use). No thread is
//! spawned and no subscription outlives the RPC — all waiter state
//! (baseline cursor, deadline) lives on the dispatch call stack, so
//! returning from dispatch frees the waiter. Each quantum also checks
//! peer liveness, so a client that disconnects mid-wait frees its
//! connection thread within one quantum instead of holding it to the
//! bound.
//!
//! Read-only with `agent_show` visibility: the answer leaves through the
//! central CAD-375 withhold, and the `approval_pending` cause is skipped
//! for callers `agent_requests` would refuse (CAD-506).
//!
//! Deliberately NOT consulted (CAD-987 is out of scope): the stall
//! streak (`stalled`), `silent_ended`, and `delivery_stalled` — the
//! silent-end streak has no hysteresis yet, and inheriting that edge
//! would make waits nondeterministic.

use super::*;

use std::time::{Duration, Instant};

/// Daemon-side wait bound (seconds). PM turns routinely outlive the
/// 600 s `agent_ask` cap, so the wait allows an hour; the CLI default
/// is 600 s. Every hold is bounded by this clamp.
pub(crate) const MAX_WAIT_SECS: u64 = 3600;
/// Re-check quantum: a `wake()` on any finish/stop/request path returns
/// the waiter within microseconds; the quantum is only the fallback for
/// a missed notify, still far inside the 1 s acceptance bound.
const QUANTUM: Duration = Duration::from_millis(250);

/// Clamp a caller-supplied bound into `[0, MAX_WAIT_SECS]`. `0` is the
/// single-evaluation probe: check once, never block.
pub(crate) fn clamp_wait_secs(want: u64) -> u64 {
    want.min(MAX_WAIT_SECS)
}

/// The `--until` conditions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitUntil {
    Reported,
    Idle,
    Attention,
    Any,
}

impl WaitUntil {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "reported" => Ok(Self::Reported),
            "idle" => Ok(Self::Idle),
            "attention" => Ok(Self::Attention),
            "any" => Ok(Self::Any),
            _ => Err(Error::rejected(format!(
                "Invalid --until '{text}' — one of reported|idle|attention|any"
            ))),
        }
    }

    fn watches_attention(self) -> bool {
        matches!(self, Self::Attention | Self::Any)
    }

    fn watches_reported(self) -> bool {
        matches!(self, Self::Reported | Self::Any)
    }

    fn watches_idle(self) -> bool {
        matches!(self, Self::Idle | Self::Any)
    }
}

/// What one evaluation found. `reason` is the output reason string.
enum WaitHit {
    Attention {
        reason: &'static str,
        message: Option<String>,
    },
    Reported {
        message: String,
    },
    /// Q5: the turn ended terminally without a report
    /// (`interrupted|unknown|cancelled`). The wait ends rather than
    /// hanging to a retryable timeout on a terminal state; `reported`
    /// stays honest (a result exists).
    Settled {
        message: String,
    },
    Idle,
}

/// One evaluation's inputs, read together. Pure data so the predicate
/// below unit-tests without a daemon.
struct WaitSnapshot {
    state: String,
    error: String,
    unknown: Vec<String>,
    /// Stall-view pane menu line (`pane_menu` on `agent_show` — public).
    menu: Option<String>,
    /// Brokered requests pending AND visible to this caller (CAD-506).
    open_requests: bool,
    /// The live turn, if any: `(message id, turn token)`.
    running: Option<(String, Option<String>)>,
    /// First terminal finish after the baseline: `(message id, state)`.
    finished_after: Option<(String, String)>,
}

/// The predicate: attention first, then reported/settled, then idle —
/// so `any` names the most actionable cause when several hold. Each arm
/// is one contract cause; removing one must fail its unit test.
fn decide(until: WaitUntil, snap: &WaitSnapshot) -> Option<WaitHit> {
    if until.watches_attention() {
        // Fence: the `attention` state, or unreconciled `unknown` rows
        // even if the state has not landed yet — belt and braces on the
        // same fence the `unknown` write and the state write form.
        if snap.state == "attention" || !snap.unknown.is_empty() {
            return Some(WaitHit::Attention {
                reason: "fence",
                message: snap
                    .unknown
                    .first()
                    .cloned()
                    .or_else(|| snap.running.as_ref().map(|(id, _)| id.clone())),
            });
        }
        if snap.menu.is_some() {
            return Some(WaitHit::Attention {
                reason: "approval_menu",
                message: snap.running.as_ref().map(|(id, _)| id.clone()),
            });
        }
        if snap.open_requests {
            return Some(WaitHit::Attention {
                reason: "approval_pending",
                message: snap.running.as_ref().map(|(id, _)| id.clone()),
            });
        }
        // The lane classifier's rate-limit arm, applied to the agent's
        // error text (null quota/usage blobs degrade exactly to that).
        if lane_rpc::pressure_label(&Value::Null, &Value::Null, &snap.error) == Some("rate-limited")
        {
            return Some(WaitHit::Attention {
                reason: "rate_limited",
                message: snap.running.as_ref().map(|(id, _)| id.clone()),
            });
        }
        if matches!(snap.state.as_str(), "stopping" | "stopped" | "offline") {
            return Some(WaitHit::Attention {
                reason: "stopped",
                message: snap.running.as_ref().map(|(id, _)| id.clone()),
            });
        }
    }
    if until.watches_reported() {
        if let Some((id, state)) = &snap.finished_after {
            if matches!(state.as_str(), "completed" | "failed") {
                return Some(WaitHit::Reported {
                    message: id.clone(),
                });
            }
            return Some(WaitHit::Settled {
                message: id.clone(),
            });
        }
    }
    if until.watches_idle() && snap.state == "idle" {
        return Some(WaitHit::Idle);
    }
    None
}

impl Shared {
    /// `agent_wait`: block (daemon-side) until the condition holds.
    /// Read-only: no store write on any path.
    pub(super) fn rpc_wait(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let until = WaitUntil::parse(optional_str(params, "until").unwrap_or("any"))?;
        let message = match optional_str(params, "message") {
            Some(id) => {
                if !until.watches_reported() {
                    return Err(Error::rejected(
                        "--message narrows --until reported|any — it names the \
                         turn to wait for, which idle|attention never consult",
                    ));
                }
                let row = self
                    .store
                    .message(id)?
                    .ok_or_else(|| Error::rejected(format!("Unknown message '{id}'")))?;
                if row.alias != alias {
                    return Err(Error::rejected(format!(
                        "Message '{id}' belongs to '{}', not '{alias}'",
                        row.alias
                    )));
                }
                Some(id.to_string())
            }
            None => None,
        };
        let bound = clamp_wait_secs(optional_u64(params, "timeout").unwrap_or(600));
        let cursor = self.store.event_cursor(&alias)?;
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(bound);
        loop {
            if let Some(out) = self.wait_check(&alias, until, &message, cursor, peer_pid)? {
                let mut obj = serde_json::Map::new();
                obj.insert("alias".to_string(), json!(alias));
                obj.insert("state".to_string(), json!(self.store.agent(&alias)?.state));
                let (reason, mid) = match &out {
                    WaitHit::Attention { reason, message } => (reason.to_string(), message.clone()),
                    WaitHit::Reported { message } => {
                        ("reported".to_string(), Some(message.clone()))
                    }
                    WaitHit::Settled { message } => ("settled".to_string(), Some(message.clone())),
                    WaitHit::Idle => ("idle".to_string(), None),
                };
                obj.insert("reason".to_string(), json!(reason));
                // The turn token rides the answer as text so the central
                // CAD-375 withhold redacts it for non-owners (I2); dead
                // tokens stay visible like `agent_show` rows.
                if let Some(id) = mid {
                    obj.insert("message".to_string(), json!(id.clone()));
                    if let Some(token) = self.store.message(&id)?.and_then(|m| m.turn_id) {
                        obj.insert("turn".to_string(), json!(token));
                    }
                }
                obj.insert("waited_secs".to_string(), json!(t0.elapsed().as_secs_f64()));
                return Ok(Value::Object(obj));
            }
            if Instant::now() >= deadline {
                return Err(Error::busy(format!(
                    "agent wait timed out after {bound}s: no {} for '{alias}'",
                    optional_str(params, "until").unwrap_or("any"),
                )));
            }
            // A dead peer means the client is gone (its fds died with
            // it — a reused pid merely keeps the old bounded hold), so
            // free the connection thread within one quantum. The answer
            // is unwritten; its kind never surfaces.
            if crate::peer::proc_starttime(peer_pid).is_none() {
                return Err(Error::internal(
                    "agent wait released: calling process exited",
                ));
            }
            if self.closing.load(Ordering::SeqCst) {
                return Err(Error::internal("agent wait released: daemon is closing"));
            }
            let ticket = self.changed.ticket();
            self.changed
                .wait_if_unchanged(ticket, deadline.min(Instant::now() + QUANTUM));
        }
    }

    /// One evaluation: read the snapshot, run the predicate. No writes.
    fn wait_check(
        &self,
        alias: &str,
        until: WaitUntil,
        message: &Option<String>,
        cursor: i64,
        peer_pid: u32,
    ) -> Result<Option<WaitHit>> {
        let agent = self.store.agent(alias)?;
        let unknown = self.store.unknown_messages(alias)?;
        let menu = self.stall_view(alias).and_then(|view| view.menu.clone());
        // Brokered-request existence is CAD-506-gated: callers
        // `agent_requests` would refuse never see this cause (fail
        // closed). The pane-menu line above is already public via
        // `agent_show`, so it needs no gate.
        let open_requests = self.may_see_requests(alias, peer_pid)
            && (self
                .pending
                .lock()
                .unwrap()
                .values()
                .any(|req| req.alias == alias)
                || self
                    .store
                    .platform_effects(Some(alias))?
                    .iter()
                    .any(|row| row.state == "waiting"));
        let running = self
            .store
            .running_message(alias)?
            .map(|m| (m.id, m.turn_id));
        // A `--message` wait keys off the named row's terminal state
        // (covers `cancelled`, which emits no `turn_finished`); without
        // `--message`, the first `turn_finished` after the baseline
        // cursor names the finished turn — durable, so a completion in
        // the read-then-wait gap cannot be missed.
        let finished_after = match message {
            Some(id) => {
                let current = self
                    .store
                    .message(id)?
                    .ok_or_else(|| Error::internal(format!("Message '{id}' vanished mid-wait")))?;
                is_terminal(&current.state).then(|| (id.clone(), current.state))
            }
            None => {
                let mut found = None;
                for event in self.store.events(alias, cursor, 100)? {
                    if event.kind == "turn_finished" {
                        if let Some(id) = event.payload.get("message").and_then(Value::as_str) {
                            if let Some(row) = self.store.message(id)? {
                                if is_terminal(&row.state) {
                                    found = Some((id.to_string(), row.state));
                                    break;
                                }
                            }
                        }
                    }
                }
                found
            }
        };
        Ok(decide(
            until,
            &WaitSnapshot {
                state: agent.state,
                error: agent.error.unwrap_or_default(),
                unknown,
                menu,
                open_requests,
                running,
                finished_after,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> WaitSnapshot {
        WaitSnapshot {
            state: "busy".to_string(),
            error: String::new(),
            unknown: Vec::new(),
            menu: None,
            open_requests: false,
            running: Some(("m1".to_string(), Some("tok-1".to_string()))),
            finished_after: None,
        }
    }

    fn reason_of(hit: Option<WaitHit>) -> Option<String> {
        match hit {
            None => None,
            Some(WaitHit::Attention { reason, .. }) => Some(reason.to_string()),
            Some(WaitHit::Reported { .. }) => Some("reported".to_string()),
            Some(WaitHit::Settled { .. }) => Some("settled".to_string()),
            Some(WaitHit::Idle) => Some("idle".to_string()),
        }
    }

    #[test]
    fn wait_until_parse_rejects_unknown() {
        assert!(WaitUntil::parse("reported").is_ok());
        assert!(WaitUntil::parse("idle").is_ok());
        assert!(WaitUntil::parse("attention").is_ok());
        assert!(WaitUntil::parse("any").is_ok());
        assert!(WaitUntil::parse("soon").is_err());
    }

    #[test]
    fn wait_clamp_bounds_the_wait() {
        assert_eq!(clamp_wait_secs(0), 0);
        assert_eq!(clamp_wait_secs(600), 600);
        assert_eq!(clamp_wait_secs(3600), 3600);
        assert_eq!(clamp_wait_secs(3601), 3600);
        assert_eq!(clamp_wait_secs(u64::MAX), 3600);
    }

    #[test]
    fn wait_fence_fires_on_attention_state_and_on_unknown_rows() {
        let mut s = snap();
        s.state = "attention".to_string();
        let hit = decide(WaitUntil::Attention, &s).expect("fence");
        assert!(matches!(
            hit,
            WaitHit::Attention {
                reason: "fence",
                ..
            }
        ));
        // Unknown rows fence even before the state lands.
        let mut s = snap();
        s.unknown = vec!["u9".to_string()];
        match decide(WaitUntil::Any, &s).expect("fence") {
            WaitHit::Attention { reason, message } => {
                assert_eq!(reason, "fence");
                assert_eq!(message, Some("u9".to_string()));
            }
            _ => panic!("wrong hit"),
        }
    }

    #[test]
    fn wait_clean_snapshot_fires_nothing() {
        assert!(decide(WaitUntil::Any, &snap()).is_none());
        assert!(decide(WaitUntil::Attention, &snap()).is_none());
        assert!(decide(WaitUntil::Reported, &snap()).is_none());
        assert!(decide(WaitUntil::Idle, &snap()).is_none());
    }

    #[test]
    fn wait_approval_menu_fires_and_names_the_running_turn() {
        let mut s = snap();
        s.menu = Some("Allow this command? [y/n]".to_string());
        match decide(WaitUntil::Attention, &s).expect("menu") {
            WaitHit::Attention { reason, message } => {
                assert_eq!(reason, "approval_menu");
                assert_eq!(message, Some("m1".to_string()));
            }
            _ => panic!("wrong hit"),
        }
    }

    #[test]
    fn wait_approval_pending_fires_only_when_requests_are_open() {
        let mut s = snap();
        s.open_requests = true;
        assert_eq!(
            reason_of(decide(WaitUntil::Attention, &s)),
            Some("approval_pending".to_string())
        );
        // The visibility gate lives in `may_see_requests` (integration
        // test proves the peer skip); the arm itself needs the flag.
        assert!(decide(WaitUntil::Attention, &snap()).is_none());
    }

    #[test]
    fn wait_rate_limited_fires_on_error_text() {
        let mut s = snap();
        s.error = "provider returned 429: rate limit exceeded, retry later".to_string();
        assert_eq!(
            reason_of(decide(WaitUntil::Attention, &s)),
            Some("rate_limited".to_string())
        );
        let mut s = snap();
        s.error = "plain boom".to_string();
        assert!(decide(WaitUntil::Attention, &s).is_none());
    }

    #[test]
    fn wait_stopped_fires_on_terminal_agent_states() {
        for state in ["stopping", "stopped", "offline"] {
            let mut s = snap();
            s.state = state.to_string();
            assert_eq!(
                reason_of(decide(WaitUntil::Attention, &s)),
                Some("stopped".to_string()),
                "{state}"
            );
        }
    }

    #[test]
    fn wait_reported_fires_on_completed_and_failed() {
        for state in ["completed", "failed"] {
            let mut s = snap();
            s.finished_after = Some(("m2".to_string(), state.to_string()));
            match decide(WaitUntil::Reported, &s).expect("reported") {
                WaitHit::Reported { message } => assert_eq!(message, "m2"),
                _ => panic!("wrong hit for {state}"),
            }
        }
    }

    #[test]
    fn wait_settled_ends_the_wait_on_unreported_terminals() {
        // Q5: interrupted/unknown/cancelled end the wait as `settled`
        // instead of hanging to a retryable timeout on a terminal state.
        for state in ["interrupted", "unknown", "cancelled"] {
            let mut s = snap();
            s.finished_after = Some(("m3".to_string(), state.to_string()));
            match decide(WaitUntil::Reported, &s).expect("settled") {
                WaitHit::Settled { message } => assert_eq!(message, "m3"),
                _ => panic!("wrong hit for {state}"),
            }
        }
    }

    #[test]
    fn wait_idle_fires_only_on_idle() {
        let mut s = snap();
        s.state = "idle".to_string();
        assert_eq!(
            reason_of(decide(WaitUntil::Idle, &s)),
            Some("idle".to_string())
        );
        assert_eq!(
            reason_of(decide(WaitUntil::Any, &s)),
            Some("idle".to_string())
        );
        assert!(decide(WaitUntil::Idle, &snap()).is_none());
    }

    #[test]
    fn wait_any_prefers_attention_over_reported_over_settled_over_idle() {
        // Everything holds: attention wins.
        let mut s = snap();
        s.state = "attention".to_string();
        s.finished_after = Some(("m2".to_string(), "completed".to_string()));
        assert_eq!(
            reason_of(decide(WaitUntil::Any, &s)),
            Some("fence".to_string())
        );
        // Attention cleared, completion + idle hold: reported wins.
        let mut s = snap();
        s.state = "idle".to_string();
        s.finished_after = Some(("m2".to_string(), "failed".to_string()));
        assert_eq!(
            reason_of(decide(WaitUntil::Any, &s)),
            Some("reported".to_string())
        );
        // Unreported terminal + idle hold: settled wins.
        let mut s = snap();
        s.state = "idle".to_string();
        s.finished_after = Some(("m3".to_string(), "interrupted".to_string()));
        assert_eq!(
            reason_of(decide(WaitUntil::Any, &s)),
            Some("settled".to_string())
        );
        // Nothing finished: idle.
        let mut s = snap();
        s.state = "idle".to_string();
        assert_eq!(
            reason_of(decide(WaitUntil::Any, &s)),
            Some("idle".to_string())
        );
    }

    #[test]
    fn wait_until_scopes_which_causes_count() {
        // Attention holds but the waiter asked for reported: nothing.
        let mut s = snap();
        s.state = "attention".to_string();
        assert!(decide(WaitUntil::Reported, &s).is_none());
        assert!(decide(WaitUntil::Idle, &s).is_none());
        // A completion holds but the waiter asked for idle: nothing.
        let mut s = snap();
        s.finished_after = Some(("m2".to_string(), "completed".to_string()));
        assert!(decide(WaitUntil::Idle, &s).is_none());
        assert!(decide(WaitUntil::Attention, &s).is_none());
    }

    /// I1: a dead peer aborts the wait instead of holding the
    /// connection thread to the bound. Without the liveness check this
    /// blocks the full 300 s; with it, it returns at once.
    #[test]
    fn wait_dead_peer_frees_the_waiter() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Shared::new(dir.path(), &ServeOptions::default()).expect("Shared::new");
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: "w",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: dir.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let t0 = Instant::now();
        let err = shared
            .dispatch(
                "agent_wait",
                &json!({"alias": "w", "until": "idle", "timeout": 300}),
                u32::MAX,
            )
            .expect_err("dead peer must release the waiter");
        assert!(err.to_string().contains("calling process exited"), "{err}");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "waiter held {:?} without liveness abort",
            t0.elapsed()
        );
    }
}
