//! `ui tailscale …` — sharing the board on the tailnet through
//! `tailscale serve` (never funnel), plus the `tailscale` CLI plumbing
//! the lifecycle start/stop uses for mapping ensure/remove. CAD-1001:
//! moved verbatim from `src/ui.rs` (the CAD-982 split, PR-2); the parent
//! re-exports what the rest of the crate uses, so the public surface of
//! `crate::ui` is unchanged.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::lifecycle::{http_get, kill_detached, read_pid, start_inner};
use super::{load_opts, save_opts, serve_opts, TailscaleAction, TailscaleOpts, UiFlags};
use crate::error::{Error, Result};
use crate::proc::{self, BoundedError};

/// One bounded `tailscale` invocation — the only way cadence talks to
/// it, and `funnel` is never among the args.
pub(crate) fn ts(args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("tailscale");
    cmd.args(args);
    proc::run_bounded(&mut cmd, Duration::from_secs(15)).map_err(|e| match e {
        BoundedError::Spawn(_) => Error::rejected("tailscale is not installed or not on PATH"),
        other => Error::internal(format!("tailscale {}: {other}", args.join(" "))),
    })
}

/// The error a dead/logged-out tailscaled produces — `ui start` warns
/// and serves loopback rather than refuse outright.
pub(crate) fn is_ts_offline(e: &Error) -> bool {
    matches!(e, Error::Rejected(m) if m.contains("tailscale"))
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
pub(crate) fn ensure_mapping(port: u16, target: &str) -> Result<bool> {
    // The tailnet is host-wide: a sandbox board never goes on it.
    crate::sandbox::refuse_global("`tailscale serve`")?;
    match serve_map()?.get(&port) {
        Some(existing) if existing == target => Ok(false),
        Some(other) => Err(Error::rejected(format!(
            "tailscale serve :{port} already targets {other} — refusing to \
             overwrite it; pick another port or free that mapping first"
        ))),
        None => {
            let out = ts(&["serve", "--bg", &format!("--https={port}"), target])?;
            if !out.status.success() {
                return Err(Error::rejected(format!(
                    "tailscale serve --https={port} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(true)
        }
    }
}

/// Remove the mapping only while it still targets what cadence
/// recorded — a foreign or absent mapping returns false.
pub(crate) fn remove_mapping(port: u16, expected: &str) -> Result<bool> {
    match serve_map()?.get(&port) {
        Some(existing) if existing == expected => {
            let out = ts(&["serve", &format!("--https={port}"), "off"])?;
            if !out.status.success() {
                return Err(Error::rejected(format!(
                    "tailscale serve --https={port} off failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
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
    crate::sandbox::refuse_global("`ui tailscale start`")?;
    let me = ts_self()?;
    let mut opts = load_opts(state_dir);
    let ui_port = opts.port.unwrap_or(3010);
    let target = format!("http://127.0.0.1:{ui_port}");
    let created = ensure_mapping(https_port, &target)?;
    opts.tailscale = Some(TailscaleOpts {
        dns_name: me.dns_name,
        https_port,
        target,
    });
    if read_only {
        opts.read_only = true;
    }
    save_opts(state_dir, &opts)?;
    // Validate before touching a running board — a persisted
    // non-loopback host must not kill it for a sharing mode that can
    // never come up.
    let _ = serve_opts(&opts)?;
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
                "mapping_created": created,
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
    let mut opts = load_opts(state_dir);
    let Some(ts) = opts.tailscale.take() else {
        println!(
            "{}",
            crate::output::json_text(&json!({"state": "not_sharing"})).unwrap_or_default()
        );
        return Ok(0);
    };
    let removed = match remove_mapping(ts.https_port, &ts.target) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("warning: {e}");
            false
        }
    };
    opts.tailscale = None;
    save_opts(state_dir, &opts)?;
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

/// Terminal QR: the `qrcode` crate (pure Rust, no other deps) plus a
/// two-rows-per-cell `▀` renderer using ANSI truecolor — dark modules
/// always black on white with a two-module quiet zone, scannable from
/// dark and light terminal themes alike.
fn qr_term(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = code.width();
    let modules = code.to_colors();
    const QUIET: usize = 2;
    let light = |x: usize, y: usize| -> bool {
        if x < QUIET || y < QUIET || x >= QUIET + w || y >= QUIET + w {
            return true;
        }
        matches!(modules[(y - QUIET) * w + (x - QUIET)], qrcode::Color::Light)
    };
    let mut out = String::new();
    let mut y = 0;
    while y < QUIET * 2 + w {
        let mut line = String::new();
        let (mut fg, mut bg) = (true, true);
        line.push_str("\x1b[38;2;255;255;255m\x1b[48;2;255;255;255m");
        for x in 0..QUIET * 2 + w {
            let (t, b) = (light(x, y), light(x, y + 1));
            if (t, b) != (fg, bg) {
                let (fv, bv) = (if t { 255 } else { 0 }, if b { 255 } else { 0 });
                line.push_str(&format!(
                    "\x1b[38;2;{fv};{fv};{fv}m\x1b[48;2;{bv};{bv};{bv}m"
                ));
                fg = t;
                bg = b;
            }
            line.push('▀');
        }
        line.push_str("\x1b[0m");
        out.push_str(&line);
        out.push('\n');
        y += 2;
    }
    Some(out)
}
