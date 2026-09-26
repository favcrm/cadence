//! CAD-535: `cadence platform` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum PlatformAction {
    /// Enroll a scoped platform credential for `account`. `token`
    /// (default): the operator-minted scoped token crosses the socket
    /// once — pass `--token-stdin` to keep it out of argv. `consent`:
    /// the platform's device-code/OTP exchange, run by the operator —
    /// only platforms with a registered consent adapter can enroll
    /// this way. Custody keeps bytes from other uids and the confined
    /// master only — same-uid unconfined agents can still read it
    /// (ADR 0006 P4), so a first enroll refuses `custody_unprotected`
    /// unless `--accept-same-uid-risk` is passed and recorded.
    Enroll {
        /// The platform (`cloudflare`, `agenticos`, …).
        platform: String,
        /// The operator's account handle, e.g. `work`, `prod`.
        #[arg(long)]
        account: String,
        /// The credential's declared scopes; repeat for several.
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        /// Exchange shape: `token` or `consent`.
        #[arg(long, default_value = "token")]
        shape: String,
        /// The scoped token itself. Prefer `--token-stdin`: argv is
        /// visible to every same-uid process.
        #[arg(long, conflicts_with = "token_stdin")]
        token: Option<String>,
        /// Read the token from stdin (one line).
        #[arg(long)]
        token_stdin: bool,
        /// Credential class — only `scoped` is enrollable; a personal
        /// approval/publish credential is refused (ADR 0006 §5.3).
        #[arg(long, default_value = "scoped")]
        class: String,
        /// Enroll even though custody is readable by same-uid managed
        /// agents today — the acceptance is recorded on the
        /// `platform_connected` audit event as
        /// `custody_risk_accepted: "same-uid"`.
        #[arg(long)]
        accept_same_uid_risk: bool,
    },
    /// Re-enroll `(platform, account)` under the same handle with a
    /// fresh credential — grants and the project default are kept.
    /// Omit `--scope` to keep the record's existing scopes.
    Rotate {
        /// The platform.
        platform: String,
        /// The enrolled account handle.
        #[arg(long)]
        account: String,
        /// The new credential's scopes; repeat for several.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// Exchange shape: `token` or `consent`.
        #[arg(long, default_value = "token")]
        shape: String,
        /// The new scoped token (argv-visible — prefer stdin).
        #[arg(long, conflicts_with = "token_stdin")]
        token: Option<String>,
        /// Read the token from stdin (one line).
        #[arg(long)]
        token_stdin: bool,
        /// Credential class — only `scoped` is enrollable.
        #[arg(long, default_value = "scoped")]
        class: String,
    },
    /// List enrolled platform accounts — handles and fingerprints,
    /// never credential bytes.
    Accounts,
    /// Revoke `(platform, account)`'s credential: its bytes drop from
    /// custody, every grant bound to it is revoked, and pending
    /// effects bound to it close unanswered.
    Revoke {
        /// The platform.
        platform: String,
        /// The enrolled account handle.
        #[arg(long)]
        account: String,
        /// Why — carried on the audit events and the closed effects.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Grant `agent` scopes on `platform`'s `account`. Repeat `--scope`.
    Grant {
        /// The agent alias granted.
        agent: String,
        /// The platform.
        platform: String,
        /// The enrolled account handle.
        #[arg(long)]
        account: String,
        /// The scopes granted; repeat for several (`*` = the account).
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
    },
    /// Revoke scopes off `agent`'s grant on `platform`/`account` —
    /// without `--scope` the whole grant goes.
    Ungrant {
        /// The agent alias.
        agent: String,
        /// The platform.
        platform: String,
        /// The enrolled account handle.
        #[arg(long)]
        account: String,
        /// The scopes revoked; repeat for several. All when omitted.
        #[arg(long = "scope")]
        scopes: Vec<String>,
    },
    /// Show grants — an agent sees only its own; the operator sees
    /// all, or `--agent`'s.
    Grants {
        /// The agent whose grants to list (operator only).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Pre-flight the grant check: is `agent`'s call at `--scope` on
    /// `platform` covered — before any platform traffic. `--account`
    /// or the project's default resolves the account.
    Check {
        /// The platform.
        platform: String,
        /// The scope the call needs.
        #[arg(long)]
        scope: String,
        /// The enrolled account handle — or `--project`/the caller's
        /// project default resolves it.
        #[arg(long)]
        account: Option<String>,
        /// The project whose default account resolves.
        #[arg(long)]
        project: Option<String>,
        /// The agent checked (operator only; an agent checks itself).
        #[arg(long)]
        agent: Option<String>,
    },
    /// List the per-project default platform accounts.
    Defaults,
    /// Set `project`'s default account for `platform`.
    DefaultSet {
        /// The project key (`cadence issue project …` lists them).
        project: String,
        /// The platform.
        platform: String,
        /// The enrolled account handle.
        #[arg(long)]
        account: String,
    },
    /// Pending platform effects and the draft log — an agent sees only
    /// its own; the operator sees all, or `--agent`'s (CAD-506).
    Effects {
        /// The agent whose effects to list (operator only).
        #[arg(long)]
        agent: Option<String>,
    },
    /// The `local` platform's outbox — the published items, or one
    /// item's rendered post with `--effect-id` (CAD-546). Operator-only.
    Outbox {
        /// One item's detail — the rendered post included.
        #[arg(long)]
        effect_id: Option<String>,
    },
    /// Cancel a staged send still `waiting`, or resolve a `reconcile`
    /// row after inspecting the platform (CAD-506). Agents close only
    /// their own waiting rows; the operator closes any.
    EffectClose {
        /// The brokered request handle — or `--effect-id`.
        #[arg(required_unless_present = "effect_id", conflicts_with = "effect_id")]
        request: Option<String>,
        /// The durable effect id.
        #[arg(long)]
        effect_id: Option<String>,
        /// Why — recorded as the row's close_reason.
        #[arg(long)]
        reason: Option<String>,
    },
}

/// The `cadence platform` tree — thin RPC wrappers (CAD-366, ADR 0006
/// §5.3). Every mutation is gated daemon-side on the caller's
/// connection (operator only); `grants`/`check` bind an agent caller
/// to its own alias. A token crosses the socket once and is never
/// echoed — `--token-stdin` keeps it out of argv.
pub(super) fn run_platform(state_dir: &Path, action: &PlatformAction) -> Result<i32> {
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    /// The credential the operator passes — `--token`'s value, or one
    /// line off stdin. Never printed, never logged.
    fn token_arg(token: &Option<String>, from_stdin: bool) -> Result<Option<String>> {
        if from_stdin {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            return Ok(Some(s.trim_end_matches(['\r', '\n']).to_string()));
        }
        Ok(token.clone())
    }
    match action {
        PlatformAction::Enroll {
            platform,
            account,
            scopes,
            shape,
            token,
            token_stdin,
            class,
            accept_same_uid_risk,
        } => {
            let mut params = json!({"platform": platform, "account": account,
                                    "scopes": scopes, "shape": shape, "class": class});
            if let Some(t) = token_arg(token, *token_stdin)? {
                params["token"] = json!(t);
            }
            if *accept_same_uid_risk {
                params["accept_same_uid_risk"] = json!(true);
            }
            print_json(&rpc("platform_enroll", params)?);
        }
        PlatformAction::Rotate {
            platform,
            account,
            scopes,
            shape,
            token,
            token_stdin,
            class,
        } => {
            let mut params = json!({"platform": platform, "account": account,
                                    "shape": shape, "class": class});
            if !scopes.is_empty() {
                params["scopes"] = json!(scopes);
            }
            if let Some(t) = token_arg(token, *token_stdin)? {
                params["token"] = json!(t);
            }
            print_json(&rpc("platform_rotate", params)?);
        }
        PlatformAction::Accounts => {
            print_json(&rpc("platform_accounts", json!({}))?);
        }
        PlatformAction::Revoke {
            platform,
            account,
            reason,
        } => {
            print_json(&rpc(
                "platform_revoke",
                json!({"platform": platform, "account": account, "reason": reason}),
            )?);
        }
        PlatformAction::Grant {
            agent,
            platform,
            account,
            scopes,
        } => {
            print_json(&rpc(
                "platform_grant",
                json!({"agent": agent, "platform": platform,
                       "account": account, "scopes": scopes}),
            )?);
        }
        PlatformAction::Ungrant {
            agent,
            platform,
            account,
            scopes,
        } => {
            let mut params = json!({"agent": agent, "platform": platform, "account": account});
            if !scopes.is_empty() {
                params["scopes"] = json!(scopes);
            }
            print_json(&rpc("platform_ungrant", params)?);
        }
        PlatformAction::Grants { agent } => {
            let mut params = json!({});
            if let Some(agent) = agent {
                params["agent"] = json!(agent);
            }
            print_json(&rpc("platform_grants", params)?);
        }
        PlatformAction::Check {
            platform,
            scope,
            account,
            project,
            agent,
        } => {
            let mut params = json!({"platform": platform, "scope": scope});
            if let Some(v) = account {
                params["account"] = json!(v);
            }
            if let Some(v) = project {
                params["project"] = json!(v);
            }
            if let Some(v) = agent {
                params["agent"] = json!(v);
            }
            print_json(&rpc("platform_check", params)?);
        }
        PlatformAction::Defaults => {
            print_json(&rpc("platform_defaults", json!({}))?);
        }
        PlatformAction::DefaultSet {
            project,
            platform,
            account,
        } => {
            print_json(&rpc(
                "platform_default_set",
                json!({"project": project, "platform": platform, "account": account}),
            )?);
        }
        PlatformAction::Effects { agent } => {
            let params = match agent {
                Some(a) => json!({"agent": a}),
                None => json!({}),
            };
            print_json(&rpc("platform_effects", params)?);
        }
        PlatformAction::Outbox { effect_id } => {
            let params = match effect_id {
                Some(id) => json!({"effect_id": id}),
                None => json!({}),
            };
            print_json(&rpc("platform_outbox", params)?);
        }
        PlatformAction::EffectClose {
            request,
            effect_id,
            reason,
        } => {
            let mut params = json!({});
            if let Some(r) = request {
                params["request"] = json!(r);
            }
            if let Some(id) = effect_id {
                params["effect_id"] = json!(id);
            }
            if let Some(r) = reason {
                params["reason"] = json!(r);
            }
            print_json(&rpc("platform_effect_close", params)?);
        }
    }
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: PlatformAction) -> Result<i32> {
    run_platform(&state_dir, &action)
}
