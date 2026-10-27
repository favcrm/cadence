//! Thread vocabulary and types: roles, entry kinds, the sender and
//! authority types, and the thread and entry rows.

use super::*;

pub const ROLE_OPERATOR: &str = "operator";
pub const ROLE_AGENT: &str = "agent";
pub const ROLE_SYSTEM: &str = "system";
/// CAD-1380: a verified organization member's own chat text — rendered
/// like an operator turn but attributed to the member, never the
/// operator, so a member message never reads as operator authority.
pub const ROLE_MEMBER: &str = "member";

pub const KIND_MESSAGE: &str = "message";
pub const KIND_ASSISTANT_TEXT: &str = "assistant_text";
pub const KIND_TOOL_CALL: &str = "tool_call";
pub const KIND_TOOL_RESULT: &str = "tool_result";
pub const KIND_TURN_RESULT: &str = "turn_result";
/// CAD-1726: a card for an agent-put canvas artifact — metadata only,
/// never the content. Never returned to a member reader.
pub const KIND_ARTIFACT: &str = "artifact";
/// CAD-1752: the master's inert proposal to hire a catalogue role - the
/// card the operator reviews. It registers nothing.
pub const KIND_HIRE_PROPOSAL: &str = "hire_proposal";

/// CAD-1767: thread entry kinds only the operator reads - artifact cards
/// and hire proposals. The one list every member-facing thread path
/// (`member_assist_history`, the board's member thread reads) filters by.
pub fn operator_only_kind(kind: &str) -> bool {
    matches!(kind, KIND_ARTIFACT | KIND_HIRE_PROPOSAL)
}

pub(super) const ROLES: &[&str] = &[ROLE_OPERATOR, ROLE_AGENT, ROLE_SYSTEM, ROLE_MEMBER];
pub(super) const KINDS: &[&str] = &[
    KIND_MESSAGE,
    KIND_ASSISTANT_TEXT,
    KIND_TOOL_CALL,
    KIND_TOOL_RESULT,
    KIND_TURN_RESULT,
    KIND_ARTIFACT,
    KIND_HIRE_PROPOSAL,
];

// ECMAScript String.trim whitespace: the Home notifier applies that same
// visible-text rule. SQLite's default trim removes only ordinary spaces.
pub(super) const REPLY_WHITESPACE: &str = concat!(
    "\t\n\u{b}\u{c}\r \u{a0}\u{1680}",
    "\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}",
    "\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}"
);

/// Entry text cap. Queued messages are at most 48 000 bytes; a longer
/// provider text is cut with a marker rather than refused.
pub const TEXT_CAP: usize = 64_000;
/// A tool call is stored as a one-line summary, never its full input.
pub const TOOL_SUMMARY_CAP: usize = 160;
/// Largest page one read returns.
pub const PAGE_MAX: i64 = 500;

/// CAD-1380 R1: the daemon-verified member authority a member send carried,
/// retained on its enqueue thread entry. Ids only, never a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberAuthority {
    pub org: String,
    pub sub: String,
    pub assistant_id: String,
    pub credential_id: String,
}

/// CAD-1568: the daemon-verified remote-CLI principal behind a hosted
/// `message_send`, retained on the message's durable row. Ids and the
/// verified claim spellings only, never a token: `sub`/`actor` are the
/// cli actor envelope's own claims, `credential_id` the issuing
/// credential's id for later authority provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliAuthority {
    pub org: String,
    /// `claims.sub` — the server-issued principal id the cli actor
    /// envelope was minted for.
    pub sub: String,
    /// `claims.actor` verbatim (`users/<h>`, `service/<h>`, `agents/<h>`
    /// or the owner's `operator`) — never a request field.
    pub actor: String,
    /// The `[A-Za-z0-9_-]` attribution handle the verifier derived —
    /// never "operator" even for an owner.
    pub handle: String,
    pub credential_id: String,
}

impl CliAuthority {
    /// The canonical `(org, sub)` key — length-prefixed like
    /// `assistant_principal_cwd`'s digest input so `("a|b","c")` and
    /// `("a","b|c")` can never encode alike and a `|`/`:` inside a claim
    /// stays unambiguous.
    pub fn principal_key(org: &str, sub: &str) -> String {
        format!("{}:{org}|{}:{sub}", org.len(), sub.len())
    }

    /// The durable sender label a cli send records on its message row —
    /// the full verified `(org, sub)` provenance, never a display name,
    /// so a principal-scoped readback compares real identity and the
    /// enqueue dedupe can never fold another principal's identical
    /// envelope into this one's.
    pub fn sender_label(&self) -> String {
        format!("cli:{}", Self::principal_key(&self.org, &self.sub))
    }
}

/// Who queued a message, for its thread entry. Only the daemon decides
/// this, from the connection — never a request field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// A caller tied to no agent. This is the operator by default, not
    /// by positive proof (CAD-313 / CAD-335 phase 2).
    Operator,
    /// The operator's chat (`thread_send`): an [`Sender::Operator`] that
    /// also starts the alias's thread — inside the enqueue transaction,
    /// so a refused message never leaves a thread behind.
    OperatorChat,
    /// A caller the daemon attributed to a registered agent.
    Agent(String),
    /// CAD-1380: a verified organization member sending to their own
    /// bound assistant. Carries the member's `sub` for attribution only
    /// — it grants no operator authority and lands in the assistant
    /// alias's own (principal-private) thread, like an ordinary send.
    Member(String),
    /// CAD-1380 R1: a member send whose ORIGINAL envelope authority was
    /// re-verified at the daemon. Built only from `member_caller`; the
    /// authority rides the enqueue entry so a queued turn re-proves it.
    VerifiedMember(MemberAuthority),
    /// CAD-1568: a remote send whose ORIGINAL cli actor envelope was
    /// re-verified at the daemon. Built only by `cli_caller`; the
    /// principal rides the durable row so a principal-scoped readback
    /// owns its messages and nothing else — it grants no operator
    /// authority.
    Cli(CliAuthority),
    /// Internal enqueues (routed results, kickoffs, monitor dispatch)
    /// and callers whose identity was not derived.
    Unattributed,
}

impl Sender {
    /// The enqueue attribution the `queued` event records (CAD-565): a
    /// stable label the delivery notice prints — who the daemon decided
    /// queued the message, never a connection detail.
    pub fn label(&self) -> &str {
        match self {
            Sender::Operator | Sender::OperatorChat => "operator",
            Sender::Agent(alias) => alias,
            Sender::Member(_) | Sender::VerifiedMember(_) => "member",
            Sender::Cli(_) => "cli",
            Sender::Unattributed => "unattributed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    /// The live alias; archived threads are never returned by alias.
    pub alias: String,
    pub created: f64,
    pub updated: f64,
    /// CAD-1098: the app installation an app conversation belongs to.
    /// `None` is the home thread (the master's own chat).
    pub install_id: Option<String>,
    /// The context a conversation was created (and, for a subject,
    /// proven) under. Binding only for a subject conversation.
    pub context_id: Option<String>,
    /// `campaign:<id>` for a campaign conversation, else `None`.
    pub subject: Option<String>,
    pub is_general: bool,
    pub archived: bool,
    pub title: Option<String>,
}

impl Thread {
    pub fn info(&self) -> cadence_core::board_api::ThreadInfo {
        cadence_core::board_api::ThreadInfo {
            id: self.id.clone(),
            alias: self.alias.clone(),
            created: self.created,
            updated: self.updated,
            install_id: self.install_id.clone(),
            context_id: self.context_id.clone(),
            subject: self.subject.clone(),
            general: self.is_general,
            is_general: self.is_general,
            archived: self.archived,
            title: self.title.clone(),
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self.info()).expect("thread serializes")
    }

    /// Home is the master's own chat: no installation scope.
    pub fn is_home(&self) -> bool {
        self.install_id.is_none()
    }

    /// CAD-1772: does this conversation run its master turn with the Home
    /// scope and tools (Home itself, or the Recruit conversation)? Every
    /// Home-versus-app branch asks this, never `is_home` alone.
    pub fn runs_as_home(&self) -> bool {
        self.is_home() || self.is_recruit()
    }

    /// CAD-1772: the master's Recruit conversation.
    pub fn is_recruit(&self) -> bool {
        self.install_id.as_deref() == Some(RECRUIT_INSTALL)
    }
}

/// CAD-1772: the reserved pseudo-installation of the master's single
/// Recruit conversation. It is not an app: no installation row exists,
/// and the conversation is operator-only and runs as a Home turn. The `@`
/// cannot pass `proto::identifier`, so no real installation, context or
/// app record can ever carry this id (a real install named `recruit` is
/// an ordinary app).
pub const RECRUIT_INSTALL: &str = "@recruit";

/// A campaign conversation's subject prefix.
pub const SUBJECT_CAMPAIGN: &str = "campaign:";
/// CAD-1636: a WhatsApp conversation's subject prefix
/// (`whatsapp:<conversation_id>`), bound to one door conversation.
pub const SUBJECT_WHATSAPP: &str = "whatsapp:";
/// At most this many conversations per (alias, installation): a
/// "New conversation" button must not grow the table without bound.
pub const CONVERSATION_LIMIT: i64 = 200;

/// Which conversation `conversation_create` makes or finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind<'a> {
    /// The app's default conversation: idempotent per (alias, install).
    General,
    /// "New conversation", `/new`, `/clear`: always a fresh row.
    Fresh,
    /// `campaign:<id>` or `whatsapp:<id>`: idempotent per (alias,
    /// install, subject).
    Subject(&'a str),
}

#[derive(Debug, Clone)]
pub struct ThreadEntry {
    pub seq: i64,
    pub thread_id: String,
    pub role: String,
    pub kind: String,
    pub text: String,
    pub payload: Option<Value>,
    pub message_id: Option<String>,
    pub created: f64,
}

impl ThreadEntry {
    pub fn row(&self) -> cadence_core::board_api::ThreadEntryRow {
        cadence_core::board_api::ThreadEntryRow {
            seq: self.seq,
            thread: self.thread_id.clone(),
            role: self.role.clone(),
            kind: self.kind.clone(),
            text: self.text.clone(),
            payload: self.payload.clone(),
            message: self.message_id.clone(),
            created: self.created,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self.row()).expect("thread entry serializes")
    }
}

/// Provider text held back until its message finishes: kept only if
/// the turn result does not carry it (CAD-320).
#[derive(Debug, Clone)]
pub struct HeldText {
    pub(super) text: String,
    pub(super) payload: Value,
}

/// One entry to append. `text` and every string in `payload` are
/// redacted and bounded on the way in.
#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    pub role: &'a str,
    pub kind: &'a str,
    pub text: &'a str,
    pub payload: Option<Value>,
    pub message_id: Option<&'a str>,
}
