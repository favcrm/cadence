//! CAD-2378: the company time zone. Every schedule runs in it. Reading it is
//! open to any caller (it is not a secret and the board shows it to every
//! member); setting it is the operator's alone.

use serde_json::{json, Value};

use super::*;
use crate::error::{Error, Result};

impl Shared {
    pub(super) fn rpc_company_zone_get(&self) -> Result<Value> {
        let (zone, source) = self.store.company_zone()?;
        Ok(json!({"timezone": zone.name(), "source": source.as_str()}))
    }

    pub(super) fn rpc_company_zone_set(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("company zone set", params, peer_pid)?;
        let name = params
            .get("timezone")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("company zone set requires 'timezone'"))?;
        self.store.set_company_zone(name, "operator")?;
        self.rpc_company_zone_get()
    }
}
