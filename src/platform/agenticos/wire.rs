//! Strict AOS-57 publish@2 wire shapes. Nullable fields remain required.
use serde::{Deserialize, Deserializer, Serialize};

fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Decision {
    Granted,
    Approved,
    Pending,
    Declined,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Status {
    Pending,
    Declined,
    Posted,
    Processing,
    Failed,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Authorization {
    pub key: String,
    pub decision: Decision,
    #[serde(deserialize_with = "nullable")]
    pub grant_id: Option<String>,
    pub repeated: bool,
    #[serde(deserialize_with = "nullable")]
    pub content_digest: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Published {
    pub key: String,
    pub decision: Decision,
    pub executed: bool,
    pub status: Status,
    #[serde(deserialize_with = "nullable")]
    pub permalink: Option<String>,
    pub repeated: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Toolkit {
    Facebook,
    Instagram,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ConnectionStatus {
    Active,
    Pending,
    Expired,
    Revoked,
    Error,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Connection {
    pub id: String,
    pub toolkit: Toolkit,
    pub display_name: String,
    pub status: ConnectionStatus,
    pub connected_by_name: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(deserialize_with = "nullable")]
    pub last_used_at: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Connections {
    pub connections: Vec<Connection>,
    #[serde(deserialize_with = "nullable")]
    pub cursor: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Account {
    pub external_id: String,
    pub name: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profile {
    pub toolkit: Toolkit,
    pub accounts: Vec<Account>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Post {
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub text: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub permalink: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Posts {
    pub posts: Vec<Post>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Insight {
    pub name: String,
    pub value: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Insights {
    pub insights: Vec<Insight>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Draft {
    pub connection_id: String,
    pub toolkit: Toolkit,
    pub display_name: String,
    pub caption: String,
    #[serde(deserialize_with = "nullable")]
    pub media_url: Option<String>,
}
