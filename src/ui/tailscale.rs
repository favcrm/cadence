//! Tailnet sharing through `tailscale serve` (never funnel), including
//! the serve mapping helpers used by the board lifecycle.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::lifecycle::{http_get, kill_detached, read_pid, start_inner};
use super::tailscale_serve;
use super::{
    clear_persisted_share, load_opts, load_opts_strict, qr_term, refuse_share_port_change,
    resolve_device_login, save_opts, serve_opts, validate_share_target, TailscaleAction,
    TailscaleOpts, UiFlags, UiOpts,
};
use crate::error::{Error, Result};
use crate::proc::{self, BoundedError};

// ---------- tailscale (serve only — never funnel) ----------

/// One bounded, read-only `tailscale` CLI invocation. Serve mutations use
/// the conditional LocalAPI transport in `tailscale_serve`; `funnel` is never among the args.
pub(crate) fn ts(args: &[&str]) -> Result<std::process::Output> {
    tailscale_serve::validate_socket_override()?;
    let mut cmd = Command::new("tailscale");
    if let Some(socket) = std::env::var_os("CADENCE_TAILSCALE_SOCKET") {
        cmd.arg("--socket").arg(socket);
    }
    cmd.args(args);
    proc::run_bounded(&mut cmd, Duration::from_secs(15)).map_err(|e| match e {
        BoundedError::Spawn(_) => Error::rejected("tailscale is not installed or not on PATH"),
        other => Error::internal(format!("tailscale {}: {other}", args.join(" "))),
    })
}

/// The error a dead/logged-out tailscaled produces — `ui start` warns
/// and serves loopback rather than refuse outright.
pub(crate) fn is_ts_offline(e: &Error) -> bool {
    // Only an actual LocalAPI transport outage retains the historical
    // loopback-only start fallback. CAS conflicts, missing ETags, malformed
    // responses, unsupported sockets and ownership refusals are not offline.
    matches!(e, Error::Rejected(m) if m.contains("tailscale LocalAPI offline:"))
}

pub(crate) struct TsSelf {
    pub(crate) dns_name: String,
}

/// `tailscale status --json` → the node's DNS name, with the three
/// refusal states the operator can act on named plainly.
pub(crate) fn ts_self() -> Result<TsSelf> {
    let out = ts(&["status", "--json"])?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let lower = stderr.to_lowercase();
        if lower.contains("logged out") {
            return Err(Error::rejected(
                "tailscale is logged out — run `tailscale up` first",
            ));
        }
        return Err(Error::rejected(format!(
            "tailscale status failed: {}",
            stderr.trim()
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::internal(format!("tailscale status --json: {e}")))?;
    let state = v["BackendState"].as_str().unwrap_or_default();
    if state != "Running" {
        return Err(Error::rejected(format!(
            "tailscale is not up (BackendState {state:?}) — run `tailscale up` first"
        )));
    }
    let dns = v["Self"]["DNSName"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_string();
    if dns.is_empty() {
        return Err(Error::rejected(
            "tailscale reports no DNS name — is this node logged in?",
        ));
    }
    let cert_domains = v["CertDomains"].as_array().cloned().unwrap_or_default();
    if cert_domains.is_empty() {
        return Err(Error::rejected(
            "HTTPS certificates are not enabled for this tailnet — enable \
             them in the admin console (DNS → HTTPS Certificates) first",
        ));
    }
    Ok(TsSelf { dns_name: dns })
}

/// `tailscale serve status --json` → https port → proxy target.
/// `Web` keys are `<dns>:<port>` (bare `<dns>` is port 443).
fn serve_map() -> Result<HashMap<u16, String>> {
    let out = ts(&["serve", "status", "--json"])?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "tailscale serve status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::internal(format!("tailscale serve status --json: {e}")))?;
    let mut map = HashMap::new();
    if let Some(web) = v["Web"].as_object() {
        for (key, entry) in web {
            let port: u16 = key
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(443);
            if let Some(target) = entry["Handlers"]["/"]["Proxy"].as_str() {
                map.insert(port, target.to_string());
            }
        }
    }
    Ok(map)
}

/// Ensure `https:<port>` proxies to `target`: identical mapping is
/// left alone (returns false), a different one on that port is a hard
/// refusal — cadence never overwrites somebody else's serve config.
fn mapping_matches(dns: &str, port: u16, target: &str) -> Result<bool> {
    tailscale_serve::matches(dns, port, target)
}

pub(crate) fn prepare_mapping(
    dns: &str,
    port: u16,
    target: &str,
) -> Result<tailscale_serve::PreparedUpdate> {
    // Preserve the exact caller hard gate before any LocalAPI connection.
    crate::sandbox::refuse_global_unless_allowed("`tailscale serve`")?;
    tailscale_serve::prepare_ensure(dns, port, target)
}

pub(crate) fn ensure_mapping(dns: &str, port: u16, target: &str) -> Result<bool> {
    prepare_mapping(dns, port, target)?.apply()
}

/// Remove the mapping only while it still targets what cadence
/// recorded — a foreign or absent mapping returns false.
pub(crate) fn remove_mapping(dns: &str, port: u16, expected: &str) -> Result<bool> {
    crate::sandbox::refuse_global_unless_allowed("`tailscale serve off`")?;
    tailscale_serve::remove(dns, port, expected)
}

// ---------- `ui tailscale …` ----------

pub(crate) fn tailscale_cli(state_dir: &Path, action: &TailscaleAction) -> Result<i32> {
    match action {
        TailscaleAction::Start { port, read_only } => ts_start(state_dir, *port, *read_only),
        TailscaleAction::Stop => ts_stop(state_dir),
        TailscaleAction::Status => ts_status(state_dir),
    }
}

/// `ui tailscale start` — the whole flow: resolve the tailnet
/// identity, ensure the mapping, persist, (re)start the board so the
/// new Host/Origin allowlists are live, print the URL.
fn ts_start(state_dir: &Path, https_port: u16, read_only: bool) -> Result<i32> {
    ts_start_inner(state_dir, https_port, read_only, false)
}

/// `ui tailscale start` with no stdout — for composed callers
/// (session's `--fix`).
pub(crate) fn ts_start_quiet(state_dir: &Path, https_port: u16, read_only: bool) -> Result<i32> {
    ts_start_inner(state_dir, https_port, read_only, true)
}

fn ts_start_inner(state_dir: &Path, https_port: u16, read_only: bool, quiet: bool) -> Result<i32> {
    crate::sandbox::refuse_global_unless_allowed("`ui tailscale start`")?;
    let mut opts = load_opts_strict(state_dir)?;
    validate_share_target(&opts)?;
    refuse_share_port_change(opts.tailscale.as_ref(), https_port)?;
    let me = ts_self()?;
    let ui_port = opts.port.unwrap_or(3010);
    crate::sandbox::refuse_production_port(ui_port)?;
    let target = format!("http://127.0.0.1:{ui_port}");
    opts.tailscale = Some(TailscaleOpts {
        dns_name: me.dns_name.clone(),
        https_port,
        target: target.clone(),
    });
    if read_only {
        opts.read_only = true;
    }
    // Validate configuration and device-login inputs before saving,
    // publishing a map, or stopping a running board.
    let _ = serve_opts(&opts)?;
    resolve_device_login(&UiFlags::default())?;
    let mapping_created = !mapping_matches(&me.dns_name, https_port, &target)?;
    save_opts(state_dir, &opts)?;
    let was_running = read_pid(state_dir).is_some();
    if was_running {
        eprintln!("restarting the board so the tailnet allowlists take effect — brief outage");
        kill_detached(state_dir);
    }
    let code = start_inner(state_dir, &UiFlags::default(), false, true)?;
    let ts = opts.tailscale.as_ref().expect("set above");
    if !quiet {
        println!(
            "{}",
            crate::output::json_text(&json!({
                "state": "sharing",
                "tailnet_url": ts.url(),
                "mapping": format!("https:{} → {}", ts.https_port, ts.target),
                "mapping_created": mapping_created,
                "board": if was_running { "restarted" } else { "started" },
                "read_only": opts.read_only,
            }))
            .unwrap_or_default()
        );
    }
    Ok(code)
}

/// `ui tailscale stop` — remove only cadence's mapping, drop the
/// tailnet options, restart the board local-only when it runs.
fn ts_stop(state_dir: &Path) -> Result<i32> {
    let mut opts = load_opts_strict(state_dir)?;
    if let Some(share) = opts.tailscale.as_ref() {
        let mut bind = UiOpts {
            host: opts.host.clone(),
            port: opts.port,
            ..UiOpts::default()
        };
        bind.tailscale = Some(share.clone());
        validate_share_target(&bind)?;
        crate::sandbox::refuse_global_unless_allowed("`ui tailscale stop`")?;
    }
    let Some(ts) = opts.tailscale.take() else {
        println!(
            "{}",
            crate::output::json_text(&json!({"state": "not_sharing"})).unwrap_or_default()
        );
        return Ok(0);
    };
    let removed = remove_mapping(&ts.dns_name, ts.https_port, &ts.target).map_err(|e| {
        Error::rejected(format!(
            "{e} — the tailnet record is kept; fix the failure and retry"
        ))
    })?;
    clear_persisted_share(state_dir)?;
    let was_running = read_pid(state_dir).is_some();
    if was_running {
        eprintln!("restarting the board local-only — brief outage");
        kill_detached(state_dir);
        start_inner(state_dir, &UiFlags::default(), false, true)?;
    }
    println!(
        "{}",
        crate::output::json_text(&json!({
            "state": "stopped_sharing",
            "mapping_removed": removed,
            "board": if was_running { "restarted" } else { "not_running" },
        }))
        .unwrap_or_default()
    );
    Ok(0)
}

/// `ui tailscale status` — sharing state, the URL, the live mapping,
/// the identity a test request resolves to, and the QR.
fn ts_status(state_dir: &Path) -> Result<i32> {
    let opts = load_opts(state_dir);
    let Some(ts) = &opts.tailscale else {
        println!("tailscale sharing: off");
        return Ok(0);
    };
    let url = ts.url();
    println!("tailscale sharing: on");
    println!("url:      {url}");
    match serve_map() {
        Ok(map) => match map.get(&ts.https_port) {
            Some(t) if t == &ts.target => {
                println!("mapping:  https:{} → {}  (live)", ts.https_port, t)
            }
            Some(t) => println!(
                "mapping:  https:{} → {}  (NOT ours — left alone)",
                ts.https_port, t
            ),
            None => println!("mapping:  https:{} absent", ts.https_port),
        },
        Err(e) => println!("mapping:  unknown — {e}"),
    }
    println!(
        "mode:     {}",
        if opts.read_only {
            "read-only"
        } else {
            "writable"
        }
    );
    // The identity probe (CAD-336): a local request shaped like the
    // proxy's — loopback peer, tailnet Host, a login header — is NOT
    // the proxy and must resolve to the plain operator. The real login
    // shows only through the tailnet URL (`<url>/api/meta`).
    if read_pid(state_dir).is_some() {
        let ui_port = opts.port.unwrap_or(3010);
        let host_hdr = format!("{}:{}", ts.dns_name, ts.https_port);
        let forged = "forged-probe@cadence.invalid";
        let login_hdr = format!("Tailscale-User-Login: {forged}");
        match http_get(
            "127.0.0.1",
            ui_port,
            "/api/meta",
            &host_hdr,
            &[login_hdr.as_str()],
        ) {
            Ok((200, body)) => {
                let meta = serde_json::from_str::<Value>(&body).unwrap_or_default();
                let actor = meta["actor"].as_str().unwrap_or_default();
                let check = meta["tailnet_proof"]["check"].as_str().unwrap_or("?");
                if actor.contains(forged) {
                    println!(
                        "identity: FORGEABLE — a local process posing as the proxy \
                         resolved to {actor}"
                    );
                } else {
                    println!(
                        "identity: local forged login ignored ({actor}; refused by \
                         check {check}); tailnet logins resolve only via {url}/api/meta"
                    );
                    println!(
                        "advice:   `cadence doctor --host` runs the whole tailnet \
                         proof up front and prints every remedy in order"
                    );
                }
            }
            Ok((code, _)) => println!("identity: probe answered http {code}"),
            Err(e) => println!("identity: probe failed — {e}"),
        }
    } else {
        println!("identity: board not running — probe skipped");
    }
    match qr_term(&url) {
        Some(qr) => print!("{qr}"),
        None => println!("(qr encode failed — the URL above still works)"),
    }
    Ok(0)
}
