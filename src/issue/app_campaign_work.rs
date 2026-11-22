//! CAD-1827: the host half of the CRM `crm-campaign-work/v1` contract.
//!
//! The CRM app owns the campaign work record and its read contract
//! (`docs/campaign-work-contract.md` in `favcrm/cadence-app-crm`). This
//! module is the host's rule adapter for it: value grammar, the pure
//! write application (revision, attribution, content binding), the
//! derived next decision and the caller-facing read shape. Persistence
//! lives in `crate::store::app_campaign_work`; nothing here touches a
//! file, a network, an actor or a clock.
//!
//! What this is not: no approval, send, publish, credential or effect
//! verb exists here. The record references a campaign and the content
//! revision the write saw; it never copies content, proposals, send
//! history or readiness. `next_decision` is derived from the recorded
//! lifecycle plus the caller's readiness answers — the host never
//! restates the CRM readiness chain, and a caller that cannot read the
//! chain passes `None` and gets no decision instead of a guess.
//!
//! Every value a caller receives is either recorded by this source or
//! named in `unavailable`. Forbidden and stale reads carry no recorded
//! values and no content facts: a refusal is a refusal, never a
//! fallback shape.

use serde_json::{json, Value};

use crate::error::{Error, Result};

mod read;

pub use read::{
    facts_json, identity, link, next_decision, read, Access, ReadInput, ReadinessFacts,
};

/// The capability a package declares in `needs.requires.contracts` to be
/// discovered as a campaign-work source. Discovery is by this contract,
/// never by an app name.
pub const CONTRACT_NAME: &str = "crm-campaign-work";
/// The one major this host serves.
pub const CONTRACT_MAJOR: u64 = 1;
/// The wire contract tag every reply carries.
pub const CONTRACT: &str = "crm-campaign-work/v1";

pub const ASSIGNEE_MAX: usize = 80;
pub const BLOCKED_REASON_MAX: usize = 200;
pub const CAMPAIGN_ID_MAX: usize = 128;

/// The recorded-value names a read can report as missing. Order is part
/// of the wire shape.
pub(super) const UNRECORDED: [&str; 6] = [
    "assignee",
    "assigned_by",
    "assigned_at",
    "due_at",
    "status",
    "blocked_reason",
];

/// Lifecycle statuses the source accepts. `None` ("not recorded") is a
/// different fact from any status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkStatus {
    Planned,
    InProgress,
    Blocked,
    Done,
    Cancelled,
}

impl WorkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "planned" => Some(Self::Planned),
            "in_progress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One recorded work row, exactly as the contract serializes it.
/// `revision` is the write CAS token; `content_revision` is the content
/// revision the last accepted write saw, never a copy of the content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRecord {
    pub revision: i64,
    pub content_revision: Option<i64>,
    pub assignee: Option<String>,
    pub assigned_by: Option<String>,
    /// ISO-8601 UTC instant.
    pub assigned_at: Option<String>,
    pub due_at: Option<String>,
    pub status: Option<WorkStatus>,
    pub blocked_reason: Option<String>,
    /// ISO-8601 UTC instant.
    pub updated_at: String,
}

impl WorkRecord {
    /// The CAS token a caller must echo: the current revision, or 0 when
    /// the campaign has no recorded row yet.
    pub fn cas_revision(row: Option<&WorkRecord>) -> i64 {
        row.map_or(0, |row| row.revision)
    }

    /// Recorded facts only — closed keys, no derivation.
    pub fn to_json(&self) -> Value {
        json!({
            "revision": self.revision,
            "content_revision": self.content_revision,
            "assignee": self.assignee,
            "assigned_by": self.assigned_by,
            "assigned_at": self.assigned_at,
            "due_at": self.due_at,
            "status": self.status.map(WorkStatus::as_str),
            "blocked_reason": self.blocked_reason,
            "updated_at": self.updated_at,
        })
    }
}

/// Who performed a write, and when. Both come from the authenticated
/// caller, never from request text.
#[derive(Clone, Debug)]
pub struct WorkAttribution {
    pub actor: String,
    /// ISO-8601 UTC instant.
    pub at: String,
}

/// One validated write patch. `None` leaves a value untouched;
/// `Some(None)` clears it; `Some(Some(_))` records it.
#[derive(Clone, Debug, Default)]
pub struct WorkPatch {
    pub assignee: Option<Option<String>>,
    pub due_at: Option<Option<String>>,
    pub status: Option<Option<WorkStatus>>,
    pub blocked_reason: Option<Option<String>>,
}

impl WorkPatch {
    pub fn is_empty(&self) -> bool {
        self.assignee.is_none()
            && self.due_at.is_none()
            && self.status.is_none()
            && self.blocked_reason.is_none()
    }

    /// Decode a closed `campaign-work.save` body (minus scope/revision).
    /// An unknown key, a wrongly typed value, or a body that changes
    /// nothing refuses here — before any store or transaction exists.
    pub fn from_body(body: &Value) -> Result<Self> {
        let fields = body
            .as_object()
            .ok_or_else(|| Error::rejected("campaign work save must be an object"))?;
        for key in fields.keys() {
            if !matches!(
                key.as_str(),
                "assignee" | "due_at" | "status" | "blocked_reason"
            ) {
                return Err(Error::rejected(format!(
                    "campaign work save carries a forbidden field: {key}"
                )));
            }
        }
        let mut patch = WorkPatch::default();
        if let Some(value) = fields.get("assignee") {
            patch.assignee = Some(match value {
                Value::Null => None,
                Value::String(text) => {
                    check_assignee(text)?;
                    Some(text.clone())
                }
                _ => return Err(Error::rejected("An assignee is text or null.")),
            });
        }
        if let Some(value) = fields.get("due_at") {
            patch.due_at = Some(match value {
                Value::Null => None,
                Value::String(text) => {
                    check_due_at(text)?;
                    Some(text.clone())
                }
                _ => return Err(Error::rejected("A due date is text or null.")),
            });
        }
        if let Some(value) = fields.get("status") {
            patch.status = Some(match value {
                Value::Null => None,
                Value::String(text) => Some(check_work_status(text)?),
                _ => return Err(Error::rejected("A work status is text or null.")),
            });
        }
        if let Some(value) = fields.get("blocked_reason") {
            patch.blocked_reason = Some(match value {
                Value::Null => None,
                Value::String(text) => {
                    check_blocked_reason(text)?;
                    Some(text.clone())
                }
                _ => return Err(Error::rejected("A blocked reason is text or null.")),
            });
        }
        if patch.is_empty() {
            return Err(Error::rejected(
                "A campaign work save changes at least one value.",
            ));
        }
        Ok(patch)
    }
}

/// The content revision a work row is bound to. `digest` is the content
/// row's own digest — the host never recomputes it here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentFacts {
    pub revision: i64,
    pub digest: String,
}

/// The CRM campaign identity grammar (`[A-Za-z0-9_-]`, 1–128).
pub fn check_campaign_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= CAMPAIGN_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(Error::rejected(
            "campaign ID uses letters, digits, - and _ (1-128)",
        ))
    }
}

/// A recorded claim is one trimmed plain line: bounded, no markup, no
/// control characters. The same shape the campaign name uses.
fn check_plain_line(value: &str, max: usize, what: &str) -> Result<()> {
    let forbidden = value
        .chars()
        .any(|c| matches!(c, '<' | '>' | '`' | '{' | '}') || (c as u32) <= 0x1f || c == '\u{7f}');
    // The app bounds the UTF-16 length; match it rather than a code-point
    // count so both sides refuse the same value.
    let too_long = value.encode_utf16().count() > max;
    if value.is_empty() || too_long || value.trim() != value || forbidden {
        return Err(Error::rejected(format!(
            "{what} must be plain text of 1-{max} characters."
        )));
    }
    Ok(())
}

pub fn check_assignee(value: &str) -> Result<()> {
    check_plain_line(value, ASSIGNEE_MAX, "An assignee")
}

pub fn check_blocked_reason(value: &str) -> Result<()> {
    check_plain_line(value, BLOCKED_REASON_MAX, "A blocked reason")
}

pub fn check_work_status(value: &str) -> Result<WorkStatus> {
    WorkStatus::parse(value).ok_or_else(|| {
        Error::rejected("A work status is planned, in_progress, blocked, done or cancelled.")
    })
}

/// A due date is a calendar day, not an instant: `YYYY-MM-DD` naming a
/// real day. A roll-over like `2026-02-30` refuses instead of becoming a
/// different day in some other timezone.
pub fn check_due_at(value: &str) -> Result<()> {
    let valid = match value.as_bytes() {
        [y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1] => {
            let digits = |bytes: &[u8]| bytes.iter().all(u8::is_ascii_digit);
            if digits(&[*y0, *y1, *y2, *y3, *m0, *m1, *d0, *d1]) && value.len() == 10 {
                let year: i32 = value[0..4].parse().unwrap_or(0);
                let month: u32 = value[5..7].parse().unwrap_or(0);
                let day: u32 = value[8..10].parse().unwrap_or(0);
                (1900..=2999).contains(&year)
                    && (1..=12).contains(&month)
                    && day >= 1
                    && day <= days_in_month(year, month)
            } else {
                false
            }
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::rejected(
            "A due date is a real calendar day written YYYY-MM-DD.",
        ))
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// ISO-8601 UTC instants are the only timestamps this source records.
pub fn is_instant(value: &str) -> bool {
    value.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

/// Format a stored epoch-seconds value as the wire instant
/// (`YYYY-MM-DDTHH:MM:SS.sssZ`). A stored value whose instant is not
/// representable refuses rather than reporting a different time.
pub fn instant(epoch: f64) -> Result<String> {
    let millis = (epoch * 1000.0).round();
    if !millis.is_finite() || millis.abs() > i64::MAX as f64 {
        return Err(Error::internal("campaign work timestamp is out of range"));
    }
    let stamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis as i64)
        .ok_or_else(|| Error::internal("campaign work timestamp is out of range"))?;
    Ok(stamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Apply one validated patch to the current row. Pure: the store's
/// transaction wraps it.
///
/// - every accepted write increments `revision` and stamps `updated_at`;
/// - a changed assignee (including clearing it) records who did it and
///   when, so provenance always names the last assignment decision;
/// - leaving `blocked` clears the blocked reason, so a resumption never
///   keeps stale block text;
/// - `content_revision` re-binds the row to what the write saw, and
///   `None` keeps the previous binding.
pub fn apply(
    current: Option<WorkRecord>,
    patch: &WorkPatch,
    attribution: &WorkAttribution,
    content_revision: Option<i64>,
) -> Result<WorkRecord> {
    check_plain_line(&attribution.actor, ASSIGNEE_MAX, "The writing actor")?;
    if !is_instant(&attribution.at) {
        return Err(Error::rejected("The write time is not an instant."));
    }
    if content_revision.is_some_and(|revision| revision < 1) {
        return Err(Error::rejected("The content revision is invalid."));
    }
    // The decode already checked these; re-checking here keeps the pure
    // application self-defending for any other caller.
    if let Some(Some(assignee)) = &patch.assignee {
        check_assignee(assignee)?;
    }
    if let Some(Some(due_at)) = &patch.due_at {
        check_due_at(due_at)?;
    }
    if let Some(Some(reason)) = &patch.blocked_reason {
        check_blocked_reason(reason)?;
    }
    let mut next = match current {
        None => WorkRecord {
            revision: 0,
            content_revision,
            assignee: None,
            assigned_by: None,
            assigned_at: None,
            due_at: None,
            status: None,
            blocked_reason: None,
            updated_at: attribution.at.clone(),
        },
        Some(row) => WorkRecord {
            content_revision: content_revision.or(row.content_revision),
            ..row
        },
    };
    if let Some(assignee) = &patch.assignee {
        if assignee.as_deref() != next.assignee.as_deref() {
            next.assignee = assignee.clone();
            next.assigned_by = Some(attribution.actor.clone());
            next.assigned_at = Some(attribution.at.clone());
        } else {
            next.assignee = assignee.clone();
        }
    }
    if let Some(due_at) = &patch.due_at {
        next.due_at = due_at.clone();
    }
    if let Some(status) = patch.status {
        next.status = status;
        if status != Some(WorkStatus::Blocked) {
            next.blocked_reason = None;
        }
    }
    if let Some(reason) = &patch.blocked_reason {
        // The contract refuses a non-null reason unless *this request* says
        // blocked: inheriting the row's current status would let a
        // reason-only write through without the status the client must send
        // (and the panel sends it deliberately on a reason-only edit).
        if reason.is_some() && patch.status != Some(Some(WorkStatus::Blocked)) {
            return Err(Error::rejected(
                "A blocked reason needs the blocked status.",
            ));
        }
        next.blocked_reason = reason.clone();
    }
    next.revision += 1;
    next.updated_at = attribution.at.clone();
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-10-25T09:00:00.000Z";
    const LATER: &str = "2026-10-26T10:30:00.000Z";
    const DIGEST: &str = concat!(
        "sha256:",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );

    fn record() -> WorkRecord {
        WorkRecord {
            revision: 1,
            content_revision: Some(4),
            assignee: Some("Ada".into()),
            assigned_by: Some("operator".into()),
            assigned_at: Some(AT.into()),
            due_at: Some("2026-11-01".into()),
            status: Some(WorkStatus::InProgress),
            blocked_reason: None,
            updated_at: AT.into(),
        }
    }

    fn content() -> ContentFacts {
        ContentFacts {
            revision: 4,
            digest: DIGEST.into(),
        }
    }

    fn attribution(actor: &str, at: &str) -> WorkAttribution {
        WorkAttribution {
            actor: actor.into(),
            at: at.into(),
        }
    }

    fn ready() -> ReadinessFacts {
        ReadinessFacts {
            draft: Some(true),
            audience_valid: Some(true),
            sender_live: Some(true),
            test_accepted: Some(true),
        }
    }

    #[test]
    fn value_grammar_refuses_the_same_values_the_source_refuses() {
        assert!(check_assignee("Ada Lovelace").is_ok());
        for bad in ["", " Ada", "Ada ", "<Ada>", "Ada\u{0}"] {
            assert!(check_assignee(bad).is_err(), "{bad:?} must refuse");
        }
        assert!(check_assignee(&"x".repeat(80)).is_ok());
        assert!(check_assignee(&"x".repeat(81)).is_err());
        for bad in [
            "",
            "2026-2-3",
            "2026-02-30",
            "2026-13-01",
            "01-02-2026",
            " 2026-02-03",
        ] {
            assert!(check_due_at(bad).is_err(), "{bad:?} must refuse");
        }
        assert!(check_due_at("2024-02-29").is_ok());
        assert!(check_due_at("2026-02-28").is_ok());
        assert!(check_due_at("2026-02-32").is_err());
        assert!(check_blocked_reason("").is_err());
        assert!(check_blocked_reason(&"x".repeat(201)).is_err());
        assert_eq!(check_work_status("blocked").unwrap(), WorkStatus::Blocked);
        assert!(check_work_status("Blocked").is_err());
        for bad in ["", "bad id", "a/b"] {
            assert!(check_campaign_id(bad).is_err(), "{bad:?} must refuse");
        }
        assert!(check_campaign_id("welcome_1-a").is_ok());
        assert!(check_campaign_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn apply_records_attribution_revisions_and_the_content_binding() {
        let first = apply(
            None,
            &WorkPatch {
                assignee: Some(Some("Ada".into())),
                due_at: Some(Some("2026-11-01".into())),
                status: Some(Some(WorkStatus::InProgress)),
                blocked_reason: None,
            },
            &attribution("operator", AT),
            Some(4),
        )
        .unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.assignee.as_deref(), Some("Ada"));
        assert_eq!(first.assigned_by.as_deref(), Some("operator"));
        assert_eq!(first.assigned_at.as_deref(), Some(AT));
        assert_eq!(first.content_revision, Some(4));
        assert_eq!(first.due_at.as_deref(), Some("2026-11-01"));
        assert_eq!(first.updated_at, AT);

        // A due-date-only write keeps the existing assignment provenance
        // while re-binding the content revision.
        let second = apply(
            Some(first.clone()),
            &WorkPatch {
                due_at: Some(Some("2026-11-05".into())),
                ..WorkPatch::default()
            },
            &attribution("agent", LATER),
            Some(5),
        )
        .unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.assigned_by.as_deref(), Some("operator"));
        assert_eq!(second.due_at.as_deref(), Some("2026-11-05"));
        assert_eq!(second.content_revision, Some(5));

        // Unassigning is itself an attributed decision.
        let third = apply(
            Some(second),
            &WorkPatch {
                assignee: Some(None),
                ..WorkPatch::default()
            },
            &attribution("agent", LATER),
            None,
        )
        .unwrap();
        assert_eq!(third.revision, 3);
        assert_eq!(third.assignee, None);
        assert_eq!(third.assigned_by.as_deref(), Some("agent"));
        assert_eq!(third.assigned_at.as_deref(), Some(LATER));
        // A `None` content revision leaves the previous binding untouched.
        assert_eq!(third.content_revision, Some(5));
    }

    #[test]
    fn apply_clears_the_block_on_resume_and_refuses_an_orphan_reason() {
        let blocked = record_with_status(WorkStatus::Blocked, Some("waiting on legal"));
        let resumed = apply(
            Some(blocked.clone()),
            &WorkPatch {
                status: Some(Some(WorkStatus::InProgress)),
                ..WorkPatch::default()
            },
            &attribution("operator", LATER),
            Some(4),
        )
        .unwrap();
        assert_eq!(resumed.status, Some(WorkStatus::InProgress));
        assert_eq!(resumed.blocked_reason, None);

        let orphan = apply(
            Some(record()),
            &WorkPatch {
                blocked_reason: Some(Some("no owner".into())),
                ..WorkPatch::default()
            },
            &attribution("operator", LATER),
            Some(4),
        );
        assert!(orphan.is_err(), "a reason without blocked must refuse");

        // A reason-only edit of an already-blocked row carries the status.
        let reason_only = apply(
            Some(blocked),
            &WorkPatch {
                status: Some(Some(WorkStatus::Blocked)),
                blocked_reason: Some(Some("waiting on finance".into())),
                ..WorkPatch::default()
            },
            &attribution("operator", LATER),
            Some(4),
        )
        .unwrap();
        assert_eq!(
            reason_only.blocked_reason.as_deref(),
            Some("waiting on finance")
        );
        assert_eq!(reason_only.revision, 2);

        // A reason-only write against an already-blocked row still has to
        // send `status: "blocked"` itself: the current row's status is not
        // the request's claim.
        let omitted = apply(
            Some(record_with_status(
                WorkStatus::Blocked,
                Some("waiting on finance"),
            )),
            &WorkPatch {
                blocked_reason: Some(Some("waiting on legal".into())),
                ..WorkPatch::default()
            },
            &attribution("operator", LATER),
            Some(4),
        );
        assert!(omitted.is_err(), "an omitted blocked status must refuse");
    }

    fn record_with_status(status: WorkStatus, reason: Option<&str>) -> WorkRecord {
        WorkRecord {
            status: Some(status),
            blocked_reason: reason.map(str::to_string),
            ..record()
        }
    }

    #[test]
    fn save_body_is_closed_and_carries_no_authority_claim() {
        let ok = WorkPatch::from_body(&json!({"assignee": "Ada", "due_at": null})).unwrap();
        assert_eq!(ok.assignee, Some(Some("Ada".into())));
        assert_eq!(ok.due_at, Some(None));
        for body in [
            json!({"revision": 9}),
            json!({"assigned_by": "someone"}),
            json!({"next_decision": {"key": "send"}}),
            json!({"campaign_id": "welcome"}),
            json!({}),
            json!({"assignee": 5}),
            json!({"due_at": "2026-02-30"}),
            json!({"status": "Blocked"}),
        ] {
            assert!(WorkPatch::from_body(&body).is_err(), "{body} must refuse");
        }
    }

    #[test]
    fn next_decision_follows_lifecycle_first_then_the_readiness_chain() {
        let facts = ready();
        assert_eq!(
            next_decision(Some(&record_with_status(WorkStatus::Done, None)), &facts)["key"],
            "closed"
        );
        assert_eq!(
            next_decision(
                Some(&record_with_status(WorkStatus::Cancelled, None)),
                &facts
            )["key"],
            "closed"
        );
        assert_eq!(next_decision(Some(&record()), &facts)["key"], "send");
        // An unassigned campaign still shows the campaign's next decision.
        assert_eq!(next_decision(None, &facts)["key"], "send");
        let blocked = next_decision(
            Some(&record_with_status(
                WorkStatus::Blocked,
                Some("waiting on legal"),
            )),
            &facts,
        );
        assert_eq!(blocked["key"], "unblock");
        assert_eq!(blocked["reason"], "waiting on legal");
        assert!(blocked["label"]
            .as_str()
            .unwrap()
            .contains("waiting on legal"));

        for (key, facts) in [
            (
                "draft",
                ReadinessFacts {
                    draft: Some(false),
                    ..ready()
                },
            ),
            (
                "audience",
                ReadinessFacts {
                    audience_valid: Some(false),
                    ..ready()
                },
            ),
            (
                "sender",
                ReadinessFacts {
                    sender_live: Some(false),
                    ..ready()
                },
            ),
            (
                "test",
                ReadinessFacts {
                    test_accepted: Some(false),
                    ..ready()
                },
            ),
        ] {
            assert_eq!(next_decision(Some(&record()), &facts)["key"], key);
        }
        // A missing chain answer is "not recorded", never a failed check.
        assert_eq!(
            next_decision(Some(&record()), &ReadinessFacts::default())["key"],
            "unknown"
        );
    }

    #[test]
    fn an_unreadable_readiness_chain_leaves_every_row_partial() {
        // The app suppresses the decision entirely when the chain is null —
        // even for a blocked, done or cancelled row. The host always passes
        // no readiness, so this is the shipped behaviour, not a corner.
        for work in [
            record(),
            record_with_status(WorkStatus::Blocked, Some("waiting on legal")),
            record_with_status(WorkStatus::Done, None),
            record_with_status(WorkStatus::Cancelled, None),
        ] {
            let reply = read(ReadInput {
                app: "crm",
                install: "inst-1",
                context: Some("ctx-1"),
                campaign: "welcome",
                work: Some(&work),
                content: None,
                readiness: None,
                access: Access::Authorized,
                expected_revision: None,
            });
            assert_eq!(reply["state"], "partial", "{work:?}");
            assert_eq!(reply["next_decision"], Value::Null, "{work:?}");
            assert_eq!(reply["unavailable"], json!(["next_decision"]), "{work:?}");
            // The recorded values still stand: partial names a gap, it does
            // not hide the row.
            assert_eq!(reply["revision"], 1);
            assert_eq!(reply["assignment"]["assignee"], "Ada");
        }
    }

    #[test]
    fn an_authorized_current_read_returns_recorded_values() {
        let content = content();
        let facts = ready();
        let work = record();
        let reply = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: Some("ctx-1"),
            campaign: "welcome",
            work: Some(&work),
            content: Some(&content),
            readiness: Some(&facts),
            access: Access::Authorized,
            expected_revision: None,
        });
        assert_eq!(reply["contract"], CONTRACT);
        assert_eq!(reply["state"], "ok");
        assert_eq!(
            reply["source"],
            json!({"app": "crm", "install_id": "inst-1", "context_id": "ctx-1"})
        );
        assert_eq!(reply["identity"], "crm:inst-1:ctx-1:welcome");
        assert_eq!(
            reply["campaign"],
            json!({"id": "welcome", "link": "crm:campaigns/welcome"})
        );
        assert_eq!(reply["revision"], 1);
        assert_eq!(reply["content_revision"], 4);
        assert_eq!(
            reply["assignment"],
            json!({"assignee": "Ada", "assigned_by": "operator", "assigned_at": AT})
        );
        assert_eq!(reply["due_at"], "2026-11-01");
        assert_eq!(
            reply["lifecycle"],
            json!({"status": "in_progress", "blocked_reason": null, "updated_at": AT})
        );
        assert_eq!(reply["next_decision"]["key"], "send");
        assert_eq!(reply["unavailable"], json!([]));
        assert_eq!(reply["refusal"], Value::Null);
    }

    #[test]
    fn a_campaign_without_recorded_work_is_unavailable_never_inferred() {
        let content = content();
        let facts = ReadinessFacts {
            draft: Some(true),
            audience_valid: Some(false),
            sender_live: Some(true),
            test_accepted: Some(false),
        };
        let reply = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: None,
            content: Some(&content),
            readiness: Some(&facts),
            access: Access::Authorized,
            expected_revision: None,
        });
        assert_eq!(reply["state"], "unavailable");
        assert_eq!(reply["revision"], Value::Null);
        assert_eq!(reply["assignment"], Value::Null);
        assert_eq!(reply["due_at"], Value::Null);
        assert_eq!(reply["lifecycle"], Value::Null);
        assert_eq!(
            reply["unavailable"],
            json!([
                "assignee",
                "assigned_by",
                "assigned_at",
                "due_at",
                "status",
                "blocked_reason"
            ])
        );
        assert_eq!(reply["next_decision"]["key"], "audience");
        assert_eq!(reply["identity"], "crm:inst-1:-:welcome");
        assert_eq!(reply["content_revision"], 4);
    }

    #[test]
    fn partial_records_list_exactly_the_values_they_do_not_have() {
        let sparse = WorkRecord {
            assignee: None,
            assigned_by: None,
            assigned_at: None,
            due_at: None,
            status: None,
            ..record()
        };
        let facts = ready();
        let reply = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: Some(&sparse),
            content: None,
            readiness: Some(&facts),
            access: Access::Authorized,
            expected_revision: None,
        });
        assert_eq!(reply["state"], "ok");
        assert_eq!(
            reply["unavailable"],
            json!(["assignee", "assigned_by", "assigned_at", "due_at", "status"])
        );
        let no_readiness = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: Some(&record()),
            content: None,
            readiness: None,
            access: Access::Authorized,
            expected_revision: None,
        });
        assert_eq!(no_readiness["state"], "partial");
        assert_eq!(no_readiness["next_decision"], Value::Null);
        assert_eq!(no_readiness["unavailable"], json!(["next_decision"]));
    }

    #[test]
    fn forbidden_and_stale_reads_refuse_with_no_values() {
        let content = content();
        let facts = ready();
        let work = record();
        let forbidden = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: Some(&work),
            content: Some(&content),
            readiness: Some(&facts),
            access: Access::Forbidden,
            expected_revision: None,
        });
        assert_eq!(forbidden["state"], "forbidden");
        assert_eq!(forbidden["refusal"]["code"], "forbidden");
        for key in [
            "revision",
            "content_revision",
            "content",
            "assignment",
            "due_at",
            "lifecycle",
            "next_decision",
        ] {
            assert_eq!(forbidden[key], Value::Null, "{key} must not leak");
        }
        assert!(forbidden["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "next_decision"));

        let stale = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: Some(&work),
            content: Some(&content),
            readiness: Some(&facts),
            access: Access::Authorized,
            expected_revision: Some(0),
        });
        assert_eq!(stale["state"], "stale");
        assert_eq!(stale["refusal"]["code"], "stale_revision");
        // Revision tokens only — never the content facts or recorded values.
        assert_eq!(stale["revision"], 1);
        assert_eq!(stale["content_revision"], 4);
        for key in [
            "content",
            "assignment",
            "due_at",
            "lifecycle",
            "next_decision",
        ] {
            assert_eq!(stale[key], Value::Null, "{key} must not leak");
        }

        let current = read(ReadInput {
            app: "crm",
            install: "inst-1",
            context: None,
            campaign: "welcome",
            work: Some(&work),
            content: Some(&content),
            readiness: Some(&facts),
            access: Access::Authorized,
            expected_revision: Some(1),
        });
        assert_eq!(current["state"], "ok");
    }

    #[test]
    fn identical_campaign_ids_in_different_sources_never_collide() {
        let a = identity("crm", "inst-a", None, "welcome");
        let b = identity("crm", "inst-b", None, "welcome");
        let c = identity("crm", "inst-a", Some("ctx-1"), "welcome");
        assert_eq!(a, "crm:inst-a:-:welcome");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(link("crm", "welcome"), "crm:campaigns/welcome");
    }

    #[test]
    fn facts_body_is_the_closed_show_shape() {
        let content = content();
        let work = record();
        let facts = facts_json(
            "crm",
            "inst-1",
            Some("ctx-1"),
            "welcome",
            Some(&work),
            Some(&content),
        );
        let keys: Vec<&str> = facts
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["campaign_id", "content", "contract", "source", "work"]
        );
        assert_eq!(facts["work"].as_object().unwrap().len(), 9);
        assert_eq!(facts["content"], json!({"revision": 4, "digest": DIGEST}));
        let empty = facts_json("crm", "inst-1", Some("ctx-1"), "welcome", None, None);
        assert_eq!(empty["work"], Value::Null);
        assert_eq!(empty["content"], Value::Null);
    }

    #[test]
    fn instants_are_utc_and_out_of_range_stamps_refuse() {
        assert!(is_instant(AT));
        assert!(is_instant("2026-10-25T09:00:00Z"));
        assert!(!is_instant("2026-10-25T09:00:00+01:00"));
        assert!(!is_instant("yesterday"));
        assert_eq!(
            instant(1_792_920_600.0).unwrap(),
            "2026-10-25T09:30:00.000Z"
        );
        assert!(instant(f64::INFINITY).is_err());
    }
}
