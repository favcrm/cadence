//! Every committed JSON Schema the core generates, keyed by its path from the
//! repository root. One list serves the core's own staleness test and the root
//! crate's lib test, which the required CI `test` job runs on every PR and in
//! the merge queue.
use crate::board_api::{
    AgentRegisterReq, AgentsResponse, CommentReq, ConversationReq, IssueDetail, IssuesResponse,
    LinkReq, MasterState, MetaResponse, NewIssueReq, PatchReq, RefReq, ReportReq, ThreadMessageReq,
    ThreadPage,
};
use crate::evaluate::{Refused, Request, Success};
use crate::issue::IssueCard;

fn pretty(schema: schemars::Schema) -> String {
    serde_json::to_string_pretty(&schema).expect("schema serializes") + "\n"
}

/// `(path from the repository root, generated schema)` for every committed
/// contract file.
pub fn committed_schemas() -> Vec<(&'static str, String)> {
    let mut schemas = vec![
        (
            "contracts/cadence-core/v1/request.schema.json",
            pretty(schemars::schema_for!(Request)),
        ),
        (
            "contracts/cadence-core/v1/success.schema.json",
            pretty(schemars::schema_for!(Success)),
        ),
        (
            "contracts/cadence-core/v1/refused.schema.json",
            pretty(schemars::schema_for!(Refused)),
        ),
        (
            "contracts/cadence-core/v1/issue-card.schema.json",
            pretty(schemars::schema_for!(IssueCard)),
        ),
        (
            "contracts/board-api/v1/issues.schema.json",
            pretty(schemars::schema_for!(IssuesResponse)),
        ),
        (
            "contracts/board-api/v1/agents.schema.json",
            pretty(schemars::schema_for!(AgentsResponse)),
        ),
        (
            "contracts/board-api/v1/meta.schema.json",
            pretty(schemars::schema_for!(MetaResponse)),
        ),
        (
            "contracts/board-api/v1/issue-detail.schema.json",
            pretty(schemars::schema_for!(IssueDetail)),
        ),
        (
            "contracts/board-api/v1/master-state.schema.json",
            pretty(schemars::schema_for!(MasterState)),
        ),
        (
            "contracts/board-api/v1/thread-page.schema.json",
            pretty(schemars::schema_for!(ThreadPage)),
        ),
        (
            "contracts/board-api/v1/request-new-issue.schema.json",
            pretty(schemars::schema_for!(NewIssueReq)),
        ),
        (
            "contracts/board-api/v1/request-report.schema.json",
            pretty(schemars::schema_for!(ReportReq)),
        ),
        (
            "contracts/board-api/v1/request-patch-issue.schema.json",
            pretty(schemars::schema_for!(PatchReq)),
        ),
        (
            "contracts/board-api/v1/request-link.schema.json",
            pretty(schemars::schema_for!(LinkReq)),
        ),
        (
            "contracts/board-api/v1/request-ref.schema.json",
            pretty(schemars::schema_for!(RefReq)),
        ),
        (
            "contracts/board-api/v1/request-comment.schema.json",
            pretty(schemars::schema_for!(CommentReq)),
        ),
        (
            "contracts/board-api/v1/request-agent-register.schema.json",
            pretty(schemars::schema_for!(AgentRegisterReq)),
        ),
        (
            "contracts/board-api/v1/request-thread-message.schema.json",
            pretty(schemars::schema_for!(ThreadMessageReq)),
        ),
        (
            "contracts/board-api/v1/request-conversation.schema.json",
            pretty(schemars::schema_for!(ConversationReq)),
        ),
    ];
    schemas.extend(crate::connection_dispatch::schema::connection_committed_schemas());
    schemas
}
