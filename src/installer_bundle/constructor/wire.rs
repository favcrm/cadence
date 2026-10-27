//! Finite internal and paired host messages. Neither shape nor a hash creates
//! authority: production callers obtain these only on constructor-owned pipes.
use super::{refused, LineageWire, QualifiedBootstrap, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Kind {
    Prepared,
    Consume,
    ConsumedCurrent,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Installer {
    pub pid: u32,
    pub starttime: String,
    pub uid: u64,
    pub gid: u64,
    pub client_digest: String,
    pub carrier_digest: String,
    pub observer_digest: String,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Recipient {
    pub pid: u32,
    pub starttime: String,
    pub generation: String,
    pub nonce: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ChildContext {
    pub version: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub configure: String,
    pub role: String,
    pub installer: Installer,
    pub recipient: Recipient,
    pub binding_json: String,
    pub remaining_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Release {
    pub version: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub operation: String,
    pub barrier_nonce: String,
    pub receipt: String,
    pub grant: String,
}
impl Release {
    pub fn frame(&self, bootstrap: &QualifiedBootstrap) -> Result<Vec<u8>> {
        if self.version != 1
            || self.kind != "release"
            || self.operation != bootstrap.operation
            || self.barrier_nonce != bootstrap.barrier_nonce
            || self.receipt.len() > 16384
            || self.grant.len() > 16384
        {
            return Err(refused());
        }
        Ok(format!("enrolled-install-r3 {} {}\n", self.grant, self.receipt).into_bytes())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum ChildRequest {
    Install { frame: String },
    Ack { frame: String },
    Ready,
    Complete,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ChildReply {
    Release { frame: String },
    Install { frame: String },
    Installed { frame: String },
    Consumed { frame: String },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OwnerCurrent {
    pub binding_json: String,
    pub phase: String,
    pub global: String,
    pub company: String,
    pub epoch: u64,
    pub lineage: LineageWire,
    pub closure: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct OwnerResponse {
    pub version: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub operation: String,
    pub barrier_nonce: String,
    pub sequence: u64,
    pub outcome: String,
    pub current: Option<OwnerCurrent>,
}
impl OwnerResponse {
    pub(super) fn validate(
        &self,
        bootstrap: &QualifiedBootstrap,
        binding: &str,
        sequence: u64,
        requested: Kind,
    ) -> Result<()> {
        let expected = if requested == Kind::Prepared {
            "prepared"
        } else {
            "consumed"
        };
        let current = self.current.as_ref().ok_or_else(refused)?;
        if self.version != 1
            || self.kind != "owner"
            || self.operation != bootstrap.operation
            || self.barrier_nonce != bootstrap.barrier_nonce
            || self.sequence != sequence
            || self.outcome != expected
            || current.phase != expected
            || current.closure != "open"
            || current.binding_json != binding
            || current.epoch != bootstrap.launch.epoch
            || current.lineage.reference != bootstrap.lineage.reference
            || current.lineage.database_epoch != bootstrap.lineage.database_epoch
            || !super::hex(&current.global, 64)
            || !super::hex(&current.company, 64)
        {
            return Err(refused());
        }
        Ok(())
    }
}

pub(super) fn json(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| refused())
}
pub(super) fn binding(
    bootstrap: &QualifiedBootstrap,
    installer: &Installer,
    recipient: &Recipient,
) -> Result<String> {
    let a = &bootstrap.manifest.artifacts;
    let input: super::Configure = serde_json::from_value(serde_json::json!({
        "version":1,"type":"configure","operation":bootstrap.operation,
        "barrierNonce":bootstrap.barrier_nonce,"expiresAtMs":bootstrap.expires_at_ms,
        "launch": {"request": {
            "identity": identity(bootstrap),"purpose":bootstrap.launch.request.purpose,
            "challenge":bootstrap.launch.request.challenge,"image":bootstrap.launch.request.image
        },"epoch":bootstrap.launch.epoch},
        "imageAttestation":"unused-for-canonical-binding",
        "lineage":{"reference":bootstrap.lineage.reference,"databaseEpoch":bootstrap.lineage.database_epoch}
    })).map_err(|_| refused())?;
    let value = serde_json::json!({"version":1,"challenge":{
        "launch":input.launch,"recipient":recipient,
        "pins":{"source":bootstrap.manifest.source,"image":bootstrap.manifest.image,
            "helper":a.helper,"node":a.node,"piGraph":a.pi_graph,"policy":a.policy},
        "lineage":input.lineage},"installer":installer,"barrierNonce":bootstrap.barrier_nonce,
        "expiresAtMs":bootstrap.expires_at_ms});
    serde_json::to_string(&value).map_err(|_| refused())
}
fn identity(b: &QualifiedBootstrap) -> serde_json::Value {
    let i = &b.launch.request.identity;
    let mut value = serde_json::json!({"company":i.company,"instance":i.instance,
        "backend":i.backend,"tier":i.tier,"generation":i.generation});
    if let Some(lane) = &i.image_lane {
        value["imageLane"] = serde_json::json!(lane);
    }
    value
}
