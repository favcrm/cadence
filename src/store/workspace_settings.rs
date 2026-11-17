//! CAD-2378: workspace-level settings, starting with the company time zone.
//!
//! One row per setting. The company zone is the zone every schedule runs in;
//! a member's own display zone never lands here (it changes only how times are
//! shown). When no zone is stored the company zone is the first social
//! connection's, else UTC.

use chrono_tz::Tz;
use rusqlite::params;

use super::StoreConn;
use super::*;
use crate::error::{Error, Result};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS workspace_settings(
 name TEXT PRIMARY KEY, value TEXT NOT NULL,
 updated REAL NOT NULL, updated_by TEXT NOT NULL);
";

const TIMEZONE: &str = "timezone";

/// Where the effective company zone came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneSource {
    /// The owner set it in Settings.
    Workspace,
    /// No owner choice yet: the first social connection's zone.
    SocialConnection,
    /// Nothing is configured: UTC.
    Default,
}

impl ZoneSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::SocialConnection => "social_connection",
            Self::Default => "default",
        }
    }
}

/// Parse an IANA zone name, refusing anything else with a clear error.
pub fn parse_zone(name: &str) -> Result<Tz> {
    let trimmed = name.trim();
    trimmed.parse::<Tz>().map_err(|_| {
        Error::rejected(format!(
            "invalid IANA timezone '{trimmed}'; expected a named zone like 'Asia/Hong_Kong' or 'UTC'"
        ))
    })
}

impl Store {
    /// Store the company zone. The name is validated first, so a bad one
    /// never reaches the table.
    pub fn set_company_zone(&self, name: &str, actor: &str) -> Result<Tz> {
        let zone = parse_zone(name)?;
        self.write_tx(|conn| {
            conn.execute(
                "INSERT INTO workspace_settings(name,value,updated,updated_by) VALUES(?,?,?,?)
                 ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated=excluded.updated,updated_by=excluded.updated_by",
                params![TIMEZONE, zone.name(), now(), actor],
            )?;
            Ok(())
        })?;
        Ok(zone)
    }

    /// The company zone and where it came from: the owner's choice, else
    /// the first social connection's zone, else UTC.
    pub fn company_zone(&self) -> Result<(Tz, ZoneSource)> {
        let stored = self
            .conn()
            .query_opt(
                "SELECT value FROM workspace_settings WHERE name=?",
                [TIMEZONE],
                |r| r.get::<_, String>(0),
            )?
            .and_then(|name| name.parse::<Tz>().ok());
        if let Some(zone) = stored {
            return Ok((zone, ZoneSource::Workspace));
        }
        if let Some(zone) = self
            .app_binding_first_publish_zone()
            .ok()
            .flatten()
            .and_then(|name| name.parse::<Tz>().ok())
        {
            return Ok((zone, ZoneSource::SocialConnection));
        }
        Ok((Tz::UTC, ZoneSource::Default))
    }

    /// The zone a new schedule runs in: the one the caller named, else the
    /// company zone. A named zone is kept verbatim; the schedule's own
    /// validation judges it.
    pub fn schedule_zone_or_company(&self, named: Option<String>) -> Result<String> {
        match named {
            Some(zone) => Ok(zone),
            None => Ok(self.company_zone()?.0.name().to_string()),
        }
    }
}
