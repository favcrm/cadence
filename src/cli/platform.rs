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
    /// List every registered adapter's reviewed tool table (local only).
    Tools {
        /// Machine output: the RPC answer unchanged.
        #[arg(long)]
        json: bool,
        /// Resolve each platform's stored default account for this project.
        #[arg(long)]
        project: Option<String>,
    },
    /// Call one reviewed platform tool through the daemon's gate.
    Call {
        /// The tool name as `platform tools` prints it.
        tool: String,
        /// Inline JSON, `@file`, a file path, or `-` for stdin.
        #[arg(long)]
        input: String,
        /// Disambiguate when several adapters declare the same tool.
        #[arg(long)]
        platform: Option<String>,
        /// Platform account; omitted: the project default.
        #[arg(long)]
        account: Option<String>,
        /// Project whose default account applies.
        #[arg(long)]
        project: Option<String>,
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
        PlatformAction::Tools { json, project } => {
            let mut params = json!({});
            if let Some(project) = project {
                params["project"] = json!(project);
            }
            let tools = rpc("platform_tools", params)?;
            if *json {
                print_json(&tools);
            } else {
                print_platform_tools(&tools);
            }
        }
        PlatformAction::Call {
            tool,
            input,
            platform,
            account,
            project,
        } => {
            let mut listing = json!({});
            if let Some(project) = project {
                listing["project"] = json!(project);
            }
            let tools = rpc("platform_tools", listing)?;
            let (owner, direct) = resolve_platform_tool(&tools, tool, platform.as_deref())?;
            if !direct {
                return Err(Error::rejected(format!(
                    "'{tool}' on '{owner}' runs only inside an app run; it has no plain \
                     platform call"
                )));
            }
            let mut params = json!({
                "platform": owner,
                "tool": tool,
                "input": parse_platform_input(input)?,
            });
            if let Some(account) = account {
                params["account"] = json!(account);
            }
            if let Some(project) = project {
                params["project"] = json!(project);
            }
            print_json(&rpc("platform_call", params)?);
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

/// One tool's owning platform from the listing. `platform` narrows it;
/// without it the tool must have exactly one owner.
fn resolve_platform_tool(
    listing: &Value,
    tool: &str,
    platform: Option<&str>,
) -> Result<(String, bool)> {
    let mut owners = Vec::new();
    for entry in listing["platforms"].as_array().into_iter().flatten() {
        let Some(name) = entry["platform"].as_str() else {
            continue;
        };
        if platform.is_some_and(|wanted| wanted != name) {
            continue;
        }
        if let Some(decl) = entry["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|decl| decl["tool"].as_str() == Some(tool))
        {
            owners.push((name.to_string(), decl["direct"].as_bool().unwrap_or(false)));
        }
    }
    match owners.len() {
        1 => Ok(owners.remove(0)),
        0 if platform.is_some() => Err(Error::rejected(format!(
            "platform '{}' does not declare tool '{tool}'",
            platform.unwrap_or_default()
        ))),
        0 => Err(Error::rejected(format!(
            "no registered platform declares tool '{tool}' — run `cadence platform tools`"
        ))),
        _ => {
            let names: Vec<&str> = owners.iter().map(|(name, _)| name.as_str()).collect();
            Err(Error::rejected(format!(
                "'{tool}' is declared by {} platforms ({}) — pass --platform",
                owners.len(),
                names.join(", ")
            )))
        }
    }
}

/// Bounded JSON object for one platform call: inline, `@file`, a path, or
/// `-` for stdin. The daemon enforces its own bound as well.
fn parse_platform_input(raw: &str) -> Result<Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 64 * 1024;
    let bytes = if raw == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::invalid("platform_call", format!("cannot read input: {e}")))?;
        bytes
    } else if raw.trim_start().starts_with('{') {
        raw.as_bytes().to_vec()
    } else {
        let path = Path::new(raw.strip_prefix('@').unwrap_or(raw));
        let mut bytes = Vec::new();
        let file = std::fs::File::open(path).map_err(|e| {
            Error::invalid(
                "platform_call",
                format!("cannot read {}: {e}", path.display()),
            )
        })?;
        file.take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::invalid(
                    "platform_call",
                    format!("cannot read {}: {e}", path.display()),
                )
            })?;
        bytes
    };
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "platform_call",
            "platform call input exceeds 64KiB",
        ));
    }
    let input: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("platform_call", "input must be a JSON object"))?;
    if !input.is_object() {
        return Err(Error::invalid(
            "platform_call",
            "input must be a JSON object",
        ));
    }
    Ok(input)
}

fn print_platform_tools(listing: &Value) {
    println!(
        "{:<16} {:<24} {:<7} {:<6} {:<16}",
        "PLATFORM", "TOOL", "EFFECT", "DIRECT", "ACCOUNT"
    );
    for entry in listing["platforms"].as_array().into_iter().flatten() {
        let platform = entry["platform"].as_str().unwrap_or_default();
        let account = entry["default_account"].as_str().unwrap_or("-");
        for decl in entry["tools"].as_array().into_iter().flatten() {
            println!(
                "{:<16} {:<24} {:<7} {:<6} {:<16}",
                platform,
                decl["tool"].as_str().unwrap_or_default(),
                decl["effect"].as_str().unwrap_or_default(),
                if decl["direct"].as_bool() == Some(true) {
                    "yes"
                } else {
                    "app"
                },
                account,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Value {
        json!({"platforms": [
            {"platform": "account-images", "manifest_version": "cadence-account-images@1",
             "default_account": null,
             "tools": [{"tool":"generate_image","effect":"draft","scopes":["cli.write"],"label":null,"direct":false}]},
            {"platform": "agenticos", "manifest_version": "agenticos-manifest@1",
             "default_account": "work",
             "tools": [{"tool":"post_draft","effect":"draft","scopes":["draft"],"label":"Draft","direct":true},
                       {"tool":"publish_post","effect":"send","scopes":["publish"],"label":null,"direct":true}]},
            {"platform": "local", "manifest_version": null, "default_account": null,
             "tools": [{"tool":"publish","effect":"send","scopes":["publish"],"label":null,"direct":true}]}
        ]})
    }

    #[test]
    fn a_tool_resolves_to_its_one_owner() {
        let (owner, direct) = resolve_platform_tool(&listing(), "post_draft", None).unwrap();
        assert_eq!(owner, "agenticos");
        assert!(direct);
    }

    #[test]
    fn an_ambiguous_or_unknown_tool_refuses() {
        let mut list = listing();
        // A second adapter claiming the same tool name is ambiguity.
        list["platforms"][2]["tools"][0]["tool"] = json!("post_draft");
        let error =
            resolve_platform_tool(&list, "post_draft", None).expect_err("two owners must refuse");
        assert!(error.to_string().contains("agenticos, local"), "{error}");
        // The explicit platform narrows it.
        assert_eq!(
            resolve_platform_tool(&list, "post_draft", Some("local"))
                .unwrap()
                .0,
            "local"
        );
        let error =
            resolve_platform_tool(&list, "nope", None).expect_err("an unknown tool must refuse");
        assert!(error.to_string().contains("platform tools"), "{error}");
        let error = resolve_platform_tool(&listing(), "generate_image", Some("agenticos"))
            .expect_err("a platform that lacks the tool must refuse");
        assert!(error.to_string().contains("does not declare"), "{error}");
    }

    #[test]
    fn an_app_run_only_tool_is_listed_but_not_direct() {
        let (owner, direct) = resolve_platform_tool(&listing(), "generate_image", None).unwrap();
        assert_eq!(owner, "account-images");
        assert!(!direct, "account images run only inside an app run");
    }

    #[test]
    fn platform_input_accepts_inline_and_at_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            parse_platform_input(r#"{"prompt":"a cat"}"#).unwrap(),
            json!({"prompt":"a cat"})
        );
        let file = dir.path().join("input.json");
        std::fs::write(&file, r#"{"prompt":"a dog"}"#).unwrap();
        assert_eq!(
            parse_platform_input(&format!("@{}", file.display())).unwrap(),
            json!({"prompt":"a dog"})
        );
        assert!(parse_platform_input(r#"[1]"#).is_err());
        assert!(parse_platform_input("{oops").is_err());
    }
}
