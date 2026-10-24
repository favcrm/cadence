//! Every committed JSON Schema the core generates, keyed by its path from the
//! repository root. One list serves the core's own staleness test and the root
//! crate's lib test, which the required CI `test` job runs on every PR and in
//! the merge queue.
use crate::board_api::{
    AgentsResponse, IssueDetail, IssuesResponse, MasterState, MetaResponse, ThreadPage,
};
use crate::evaluate::{Refused, Request, Success};
use crate::issue::IssueCard;

fn pretty(schema: schemars::Schema) -> String {
    serde_json::to_string_pretty(&schema).expect("schema serializes") + "\n"
}

/// `(path from the repository root, generated schema)` for every committed
/// contract file.
pub fn committed_schemas() -> Vec<(&'static str, String)> {
    vec![
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
    ]
}
