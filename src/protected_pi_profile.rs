//! Shared protected helper wire grammar. Routing tokens are not launch authority.
//! No caller executable, script, extension path, fd, uid, hash or environment.
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// Fixed image topology. Digests/entrypoints come only from the authenticated
/// private image owner, never a candidate capture or a helper caller.
pub(crate) const IMAGE_ROOT: &str = "/opt/cadence/pi";
pub(crate) const NODE_PATH: &str = "/opt/cadence/pi/node";
#[path = "adapter/pi_guest/authority.rs"]
pub(crate) mod authority;
#[path = "adapter/pi_guest/purpose.rs"]
pub(crate) mod purpose;

#[derive(Debug)]
pub(crate) struct Profile {
    alias_sha256: String,
    generation: String,
    routing: Routing,
}
#[derive(Debug)]
pub(crate) struct Routing {
    no_session: bool,
    model: Option<String>,
}
fn text(value: &OsStr) -> Result<&str, String> {
    value
        .to_str()
        .ok_or_else(|| "protected tokens must be UTF-8".into())
}
fn hex_token(value: &OsStr, prefix: &str, length: usize) -> Result<String, String> {
    let token = text(value)?
        .strip_prefix(prefix)
        .ok_or("missing protected segment")?;
    if token.len() != length
        || !token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("protected segment must be fixed lowercase hex".into());
    }
    Ok(token.to_owned())
}
impl Profile {
    /// `rest` follows the legacy verb `exec`; order is fixed and no --env is accepted.
    pub(crate) fn parse(rest: &[OsString]) -> Result<Self, String> {
        if rest.len() < 7 || rest.len() > 10 || rest.iter().any(|s| s.as_bytes().len() > 256) {
            return Err("protected profile argument bounds".into());
        }
        if rest[0] != "--profile" || rest[1] != "pi-guest" || rest[4] != "--" {
            return Err("unknown or malformed protected profile".into());
        }
        Ok(Self {
            alias_sha256: hex_token(&rest[2], "--alias-sha256=", 64)?,
            generation: hex_token(&rest[3], "--generation=", 32)?,
            routing: Routing::parse(&rest[5..])?,
        })
    }
    pub(crate) fn alias_sha256(&self) -> &str {
        &self.alias_sha256
    }
    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }
    pub(crate) fn routing(&self) -> &Routing {
        &self.routing
    }
    pub(crate) fn selection(&self) -> Result<authority::Selection, String> {
        let selection = authority::Selection {
            alias_sha256: self.alias_sha256.clone(),
            generation: self.generation.clone(),
            role: if self.routing.no_session {
                authority::Role::Master
            } else {
                authority::Role::Worker
            },
            model: self
                .routing
                .model
                .clone()
                .ok_or("protected owner requires explicit model")?,
        };
        selection.validate().map_err(|e| e.to_string())?;
        Ok(selection)
    }
}
impl Routing {
    pub(crate) fn parse(args: &[OsString]) -> Result<Self, String> {
        if !(2..=5).contains(&args.len()) || args[0] != "--mode" || args[1] != "rpc" {
            return Err("protected Pi requires --mode rpc, never a caller program".into());
        }
        let mut no_session = false;
        let mut model = None;
        let mut at = 2;
        while at < args.len() {
            match text(&args[at])? {
                "--no-session" if !no_session => {
                    no_session = true;
                    at += 1;
                }
                "--model" if model.is_none() => {
                    let value = text(args.get(at + 1).ok_or("--model needs a routing name")?)?;
                    if value.is_empty()
                        || value.len() > 192
                        || value.starts_with(['/', '.', '-'])
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/_.+-".contains(&b))
                        || value
                            .split('/')
                            .any(|s| s.is_empty() || s == "." || s == "..")
                    {
                        return Err("invalid protected model routing name".into());
                    }
                    model = Some(value.to_owned());
                    at += 2;
                }
                _ => {
                    return Err(
                        "unlisted protected routing token (paths/fds/env/authority refused)".into(),
                    )
                }
            }
        }
        Ok(Self { no_session, model })
    }
    pub(crate) fn for_agent(no_session: bool, model: &str) -> Result<Self, String> {
        let mut args = vec!["--mode".into(), "rpc".into()];
        if no_session {
            args.push("--no-session".into());
        }
        args.extend(["--model".into(), model.into()]);
        Self::parse(&args)
    }
    pub(crate) fn tokens(&self) -> Vec<String> {
        let mut args = vec!["--mode".into(), "rpc".into()];
        if self.no_session {
            args.push("--no-session".into());
        }
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args
    }
    pub(crate) fn no_session(&self) -> bool {
        self.no_session
    }
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
